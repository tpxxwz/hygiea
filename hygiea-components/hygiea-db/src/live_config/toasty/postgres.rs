//! 动态配置在 PostgreSQL 上的 toasty 实现。SQL 在 `sql/postgres/live_config.*`

use serde_json::{Map, Value};

use hygiea_core::{Result, ResultExt};

use super::super::Backend;
use super::super::schema::{State, Tables, broken, check_err, created, state};
use super::super::worker::Row;
use crate::sql::postgres::DDL_LOCK;
use crate::sql::postgres::live_config::{DDL_SEPARATOR, OBJECTS, seed_param};
use crate::toasty::internal::postgres::{begin, columns, text_array};
use crate::toasty::internal::{fields, i64, opt_string, string};

pub(super) async fn load(
    db: &mut toasty::Db,
    sql: &str,
    namespace: &str,
) -> toasty::Result<Vec<Row>> {
    let rows = toasty::sql::query(sql).bind(namespace).exec(db).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut it = fields(row)?.into_iter();
        let mut next = || {
            it.next()
                .ok_or_else(|| toasty::Error::invalid_result("expected 3 columns"))
        };
        out.push((i64(next()?)?, opt_string(next()?)?, opt_string(next()?)?));
    }
    Ok(out)
}

pub(super) async fn revisions(
    db: &mut toasty::Db,
    sql: &str,
) -> toasty::Result<Vec<(String, i64)>> {
    let rows = toasty::sql::query(sql).exec(db).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut it = fields(row)?.into_iter();
        let mut next = || {
            it.next()
                .ok_or_else(|| toasty::Error::invalid_result("expected 2 columns"))
        };
        out.push((string(next()?)?, i64(next()?)?));
    }
    Ok(out)
}

/// 一条语句补完：参数是 `{namespace: {key: value}}`，在库里展开
pub(super) async fn seed(
    db: &mut toasty::Db,
    sql: &str,
    defaults: &[(String, Map<String, Value>)],
) -> toasty::Result<()> {
    toasty::sql::statement(sql)
        .bind(seed_param(defaults))
        .exec(db)
        .await?;
    Ok(())
}

pub(super) async fn trim_history(
    db: &mut toasty::Db,
    sql: &str,
    keep_rows: i64,
) -> toasty::Result<u64> {
    toasty::sql::statement(sql).bind(keep_rows).exec(db).await
}

// ---- 检查、建表 -------------------------------------------------------------------

/// 检查表和函数、触发器，没有就在事务里建（规则见 `schema.rs`）。在一条固定的连接上做，出错时回滚
pub(super) async fn ensure(db: &toasty::Db, tables: &Tables) -> Result<()> {
    let mut conn = db.connection().await.wrap_err(check_err)?;
    let backend = Backend::Postgres;
    let mut expected = tables.functions();
    expected.extend(tables.triggers(backend));
    let check = |columns: &crate::sql::Columns, objects: &[String]| {
        state(tables, backend, columns, &expected, objects)
    };

    let found = columns(&mut conn, tables.table_names().to_vec())
        .await
        .wrap_err(check_err)?;
    let objects = objects(&mut conn, tables).await.wrap_err(check_err)?;
    match check(&found, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }

    // 等建表锁不被 DBA 设的超时打断，见 sql::postgres::begin_sql
    begin(&mut conn, 0).await.wrap_err(check_err)?;
    let result = create(&mut conn, tables, &check).await;
    // 没提交的都回滚（建好了已经提交，这条只是警告）；回滚失败说明连接坏了，toasty 的池会扔掉它
    let _ = toasty::sql::statement("ROLLBACK").exec(&mut conn).await;
    result
}

/// 事务里：拿建表锁，再查一遍，还是没有就逐条执行 DDL 并提交
async fn create(
    conn: &mut toasty::Connection,
    tables: &Tables,
    check: &impl Fn(&crate::sql::Columns, &[String]) -> State,
) -> Result<()> {
    let backend = Backend::Postgres;
    toasty::sql::statement(DDL_LOCK)
        .bind(hygiea_core::builtin::LOCK_TYPE)
        .bind(hygiea_core::builtin::Lock::LiveConfigDdl as i32)
        .exec(conn)
        .await
        .wrap_err(check_err)?;
    let found = columns(conn, tables.table_names().to_vec())
        .await
        .wrap_err(check_err)?;
    let objects = objects(conn, tables).await.wrap_err(check_err)?;
    match check(&found, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }
    let ddl = tables.ddl(backend);
    let result: toasty::Result<()> = async {
        for statement in ddl.split(DDL_SEPARATOR) {
            toasty::sql::statement(statement).exec(conn).await?;
        }
        toasty::sql::statement("COMMIT").exec(conn).await?;
        Ok(())
    }
    .await;
    created(result, tables, backend)
}

/// 库里已有的同名函数、触发器
async fn objects(conn: &mut toasty::Connection, tables: &Tables) -> toasty::Result<Vec<String>> {
    let (functions, ty) = text_array(tables.functions());
    let (triggers, ty2) = text_array(tables.triggers(Backend::Postgres));
    let rows = toasty::sql::query(OBJECTS)
        .bind_typed(functions, ty)
        .bind_typed(triggers, ty2)
        .exec(conn)
        .await?;
    rows.into_iter()
        .map(|row| {
            fields(row)?
                .into_iter()
                .next()
                .ok_or_else(|| toasty::Error::invalid_result("expected 1 column"))
                .and_then(string)
        })
        .collect()
}
