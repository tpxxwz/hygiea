//! 动态配置在 MySQL 上的 sqlx 实现：MySQL 没有 LISTEN / NOTIFY，只靠轮询，直接在小池上查，断了 sqlx 下次拿连接时重连。
//! SQL 在 `sql/mysql/live_config.*`

use serde_json::{Map, Value};
use sqlx::mysql::{MySqlConnectOptions, MySqlPool};
use sqlx::{AssertSqlSafe, MySqlConnection};

use hygiea_core::{Result, ResultExt, err};

use super::super::Backend;
use super::super::schema::{State, Tables, broken, check_err, created, seed_rows, state};
use super::super::worker::Row;
use super::fixed;
use crate::sql::Columns;
use crate::sql::mysql::live_config::{
    DDL_LOCK, DDL_SEPARATOR, DDL_UNLOCK, SEED_ROWS, SESSION_SETTINGS, TRIGGER_NAMES, ddl_lock_name,
    seed_sql,
};
use crate::{PoolConfig, RepoErr};

pub(super) struct MySqlConn(MySqlPool);

impl MySqlConn {
    /// `options` 是主池的连接参数。每条连接建好时关掉 DBA 设的几个超时，见 [`SESSION_SETTINGS`]
    pub(super) fn new(options: MySqlConnectOptions, main: &PoolConfig) -> Self {
        let pool = fixed(main)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::raw_sql(SESSION_SETTINGS).execute(conn).await?;
                    Ok(())
                })
            })
            .connect_lazy_with(options);
        Self(pool)
    }

    /// `ensure_tables` 时检查表和触发器，没有就建
    pub(super) async fn open(&mut self, tables: &Tables, ensure_tables: bool) -> Result<()> {
        if ensure_tables {
            let mut conn = self
                .0
                .acquire()
                .await
                .wrap_err(|| err!(RepoErr::ExecFailed, "live config setup"))?;
            ensure(&mut conn, tables).await?;
        }
        Ok(())
    }

    pub(super) async fn load(&self, sql: &str, namespace: &str) -> sqlx::Result<Vec<Row>> {
        sqlx::query_as(AssertSqlSafe(sql))
            .bind(namespace)
            .fetch_all(&self.0)
            .await
    }

    pub(super) async fn revisions(&self, sql: &str) -> sqlx::Result<Vec<(String, i64)>> {
        sqlx::query_as(AssertSqlSafe(sql)).fetch_all(&self.0).await
    }

    /// 拼 `VALUES (?, ?, ?), ..` 和 ON DUPLICATE KEY UPDATE，在一个事务里执行（超过 [`SEED_ROWS`] 行分几条）
    pub(super) async fn seed(
        &self,
        insert: &str,
        defaults: &[(String, Map<String, Value>)],
    ) -> sqlx::Result<()> {
        let rows = seed_rows(defaults);
        let mut tx = self.0.begin().await?;
        for chunk in rows.chunks(SEED_ROWS) {
            let mut query = sqlx::query(AssertSqlSafe(seed_sql(insert, chunk.len())));
            for (namespace, key, value) in chunk {
                query = query.bind(*namespace).bind(*key).bind(value.as_str());
            }
            query.execute(&mut *tx).await?;
        }
        tx.commit().await
    }

    pub(super) async fn trim_history(&self, sql: &str, keep_rows: i64) -> sqlx::Result<u64> {
        sqlx::query(AssertSqlSafe(sql))
            .bind(keep_rows)
            .execute(&self.0)
            .await
            .map(|r| r.rows_affected())
    }

    /// 等借出去的连接还回来后关掉小池
    pub(super) async fn close(&self) {
        self.0.close().await;
    }
}

// ---- 检查、建表 -------------------------------------------------------------------

/// MySQL 的 DDL 会隐式提交，建表不能放在事务里：用 `GET_LOCK` 会话级锁串行化，建完（成功或失败）都放锁。
/// 建到一半失败时已经建好的不会回滚，下次启动按"只有一部分"拒绝启动，手动处理。
///
/// 拿锁前的检查只认"都在"：别的实例建表时一条条执行，没拿锁时可能正好看到建了一部分，这时不能当成结构不对，
/// 要等拿到锁（那边建完）再判断
async fn ensure(conn: &mut MySqlConnection, tables: &Tables) -> Result<()> {
    let backend = Backend::MySql;
    let ddl = tables.mysql_ddl();
    let expected = tables.triggers(backend);

    let columns = mysql_columns(conn, tables).await.wrap_err(check_err)?;
    let objects = mysql_objects(conn, &expected).await.wrap_err(check_err)?;
    if let State::Ready = state(tables, backend, &columns, &expected, &objects) {
        return Ok(());
    }

    let lock = ddl_lock_name();
    let got: Option<i64> = sqlx::query_scalar(DDL_LOCK)
        .bind(&lock)
        .fetch_one(&mut *conn)
        .await
        .wrap_err(check_err)?;
    if got != Some(1) {
        return Err(err!(
            RepoErr::QueryFailed,
            format!("live config check tables: GET_LOCK({lock}) returned {got:?}")
        ));
    }
    let result = create(conn, tables, &expected, &ddl).await;
    // 放锁失败（连接断了）时连接断开锁也就没了，不影响结果
    let _ = sqlx::query(DDL_UNLOCK)
        .bind(&lock)
        .execute(&mut *conn)
        .await;
    result
}

/// 拿到建表锁之后：再查一遍（多个实例同时启动，前一个可能已经建好了），都在就用，对不上拒绝启动，一个都没有就逐条执行 DDL
async fn create(
    conn: &mut MySqlConnection,
    tables: &Tables,
    expected: &[String],
    ddl: &str,
) -> Result<()> {
    let columns = mysql_columns(conn, tables).await.wrap_err(check_err)?;
    let objects = mysql_objects(conn, expected).await.wrap_err(check_err)?;
    match state(tables, Backend::MySql, &columns, expected, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, ddl),
        State::Absent => {}
    }
    let result: sqlx::Result<()> = async {
        for statement in ddl.split(DDL_SEPARATOR) {
            // CREATE TRIGGER 不能走预编译，用 raw_sql（文本协议）
            sqlx::raw_sql(AssertSqlSafe(statement))
                .execute(&mut *conn)
                .await?;
        }
        Ok(())
    }
    .await;
    created(result, tables, ddl)
}

async fn mysql_columns(conn: &mut MySqlConnection, tables: &Tables) -> sqlx::Result<Columns> {
    crate::sqlx::internal::mysql::columns(conn, tables.table_names().to_vec()).await
}

/// 当前库里已有的同名触发器
async fn mysql_objects(
    conn: &mut MySqlConnection,
    expected: &[String],
) -> sqlx::Result<Vec<String>> {
    let names: Vec<String> = sqlx::query_scalar(TRIGGER_NAMES)
        .fetch_all(&mut *conn)
        .await?;
    Ok(names
        .into_iter()
        .filter(|name| expected.contains(name))
        .collect())
}
