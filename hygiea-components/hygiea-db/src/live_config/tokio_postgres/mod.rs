//! 动态配置的 tokio-postgres 实现，一种数据库一个文件（现在只有 `postgres.rs`），这里的 [`TokioPgStore`] 只做转错误。
//! 和 sqlx 的 PG 一样：一条专用连接（不占主池）上 LISTEN 推送，轮询兜底；断了由 worker 重建（见 `worker.rs` 的
//! `rebuild_listener`）。SQL 在 `sql/postgres/live_config.*`，和 sqlx 共用

use serde_json::{Map, Value};

use hygiea_core::{Result, ResultExt, err};

use super::Backend;
use super::schema::{Sql, Tables};
use super::worker::{Loaded, Signal};
use crate::RepoErr;

mod postgres;

pub(crate) struct TokioPgStore {
    tables: Tables,
    sql: Sql,
    conn: postgres::PgConn,
}

impl TokioPgStore {
    /// `config` 是主池的连接参数
    pub(crate) fn postgres(config: tokio_postgres::Config, tables: Tables) -> Self {
        Self {
            sql: Sql::new(&tables, Backend::Postgres),
            tables,
            conn: postgres::PgConn::new(config),
        }
    }

    pub(crate) fn backend(&self) -> Backend {
        Backend::Postgres
    }

    pub(crate) fn has_push(&self) -> bool {
        true
    }

    pub(crate) fn table(&self) -> String {
        self.tables.config()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.conn.is_open()
    }

    pub(crate) async fn open(&mut self, ensure_tables: bool) -> Result<()> {
        self.conn.open(&self.tables, ensure_tables).await
    }

    pub(crate) fn reset(&mut self) -> bool {
        self.conn.reset()
    }

    pub(crate) async fn load(&mut self, namespace: &str) -> Result<Loaded> {
        let rows = self
            .conn
            .load(&self.sql.load, namespace)
            .await
            .wrap_err(|| {
                err!(
                    RepoErr::QueryFailed,
                    format!("live config load {namespace}")
                )
            })?;
        Ok(Loaded::from_rows(rows))
    }

    pub(crate) async fn revisions(&mut self) -> Result<Vec<(String, i64)>> {
        self.conn
            .revisions(&self.sql.revisions)
            .await
            .wrap_err(|| err!(RepoErr::QueryFailed, "live config revisions"))
    }

    pub(crate) async fn seed(&mut self, defaults: &[(String, Map<String, Value>)]) -> Result<()> {
        if defaults.iter().all(|(_, values)| values.is_empty()) {
            return Ok(());
        }
        self.conn
            .seed(&self.sql.seed, defaults)
            .await
            .wrap_err(|| err!(RepoErr::InsertFailed, "live config seed defaults"))
    }

    pub(crate) async fn trim_history(&mut self, keep_rows: u64) -> Result<u64> {
        // 配置里的数字很小，转不过去就用最大值（等于不删）
        let keep_rows = i64::try_from(keep_rows).unwrap_or(i64::MAX);
        self.conn
            .trim_history(&self.sql.trim_history, keep_rows)
            .await
            .wrap_err(|| err!(RepoErr::DeleteFailed, "live config trim history"))
    }

    pub(crate) async fn recv(&mut self) -> Result<Signal> {
        self.conn.recv().await
    }

    pub(crate) async fn close(&mut self) {
        self.conn.close();
    }
}
