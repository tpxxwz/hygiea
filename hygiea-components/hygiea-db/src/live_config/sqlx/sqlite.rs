//! 动态配置在 SQLite 上的 sqlx 实现：没有推送，只靠轮询，直接在小池上查，断了 sqlx 下次拿连接时重连。SQL 在 `sql/sqlite/live_config.*`

use serde_json::{Map, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use sqlx::{AssertSqlSafe, Connection, SqliteConnection};

use hygiea_core::{Result, ResultExt, err};

use super::super::Backend;
use super::super::schema::{State, Tables, broken, check_err, created, seed_rows, state};
use super::super::worker::Row;
use super::fixed;
use crate::sql::Columns;
use crate::sql::sqlite::live_config::{SEED_ROWS, TRIGGER_NAMES, seed_sql};
use crate::{PoolConfig, RepoErr};

pub(super) struct SqliteConn(SqlitePool);

impl SqliteConn {
    pub(super) fn new(options: SqliteConnectOptions, main: &PoolConfig) -> Self {
        Self(fixed(main).connect_lazy_with(options))
    }

    /// `ensure_tables` 时检查表和触发器，没有推送要建
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

    /// 拼 `VALUES (?, ?, ?), ..` 和 ON CONFLICT，在一个事务里执行（超过 [`SEED_ROWS`] 行分几条）
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

async fn ensure(conn: &mut SqliteConnection, tables: &Tables) -> Result<()> {
    let backend = Backend::Sqlite;
    let ddl = tables.sqlite_ddl();
    let expected = tables.triggers(backend);
    let check =
        |columns: &Columns, objects: &[String]| state(tables, backend, columns, &expected, objects);

    let columns = sqlite_columns(conn, tables).await.wrap_err(check_err)?;
    let objects = sqlite_objects(conn, &expected).await.wrap_err(check_err)?;
    match check(&columns, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, &ddl),
        State::Absent => {}
    }

    // IMMEDIATE：开事务就拿写锁，别的进程同时建表要等这边提交
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .wrap_err(check_err)?;
    let columns = sqlite_columns(&mut tx, tables).await.wrap_err(check_err)?;
    let objects = sqlite_objects(&mut tx, &expected)
        .await
        .wrap_err(check_err)?;
    match check(&columns, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, &ddl),
        State::Absent => {}
    }
    let result = async {
        sqlx::raw_sql(AssertSqlSafe(ddl.as_str()))
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    created(result, tables, &ddl)
}

async fn sqlite_columns(conn: &mut SqliteConnection, tables: &Tables) -> sqlx::Result<Columns> {
    crate::sqlx::internal::sqlite::columns(conn, tables.table_names().to_vec()).await
}

/// 库里已有的同名触发器（挂在哪张表上都算）
async fn sqlite_objects(
    conn: &mut SqliteConnection,
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
