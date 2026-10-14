//! 动态配置的 toasty 实现，一种数据库一个文件（现在只有 `postgres.rs`），这里的 [`ToastyStore`] 只做转错误。
//! SQL 在 `sql/<数据库>/live_config.*`，和 sqlx 的实现共用。
//!
//! 和 sqlx 的实现不同的地方：
//! - toasty 没有暴露 PG 的 LISTEN / NOTIFY，只靠轮询（默认 1 秒）
//! - 不另开小池，用连接池自己的 `Db`（toasty 的池），每次查询从池里拿一条连接，占主池的名额
//! - 原生 SQL 走预编译，一次只能执行一条：建表的 DDL 按 `-- @@` 拆开逐条执行

use serde_json::{Map, Value};

use hygiea_core::{Result, ResultExt, err};

use super::Backend;
use super::schema::{Sql, Tables};
use super::worker::{Loaded, Signal};
use crate::RepoErr;

mod postgres;

pub(crate) struct ToastyStore {
    tables: Tables,
    sql: Sql,
    db: toasty::Db,
}

impl ToastyStore {
    pub(crate) fn postgres(db: toasty::Db, tables: Tables) -> Self {
        Self {
            sql: Sql::new(&tables, Backend::Postgres),
            tables,
            db,
        }
    }

    pub(crate) fn backend(&self) -> Backend {
        Backend::Postgres
    }

    pub(crate) fn has_push(&self) -> bool {
        false
    }

    pub(crate) fn table(&self) -> String {
        self.tables.config()
    }

    /// 没有要建的连接，一直能用
    pub(crate) fn is_open(&self) -> bool {
        true
    }

    pub(crate) async fn open(&mut self, ensure_tables: bool) -> Result<()> {
        if ensure_tables {
            postgres::ensure(&self.db, &self.tables).await?;
        }
        Ok(())
    }

    /// 没有要丢掉的连接（toasty 的池自己处理坏连接）
    pub(crate) fn reset(&mut self) -> bool {
        false
    }

    pub(crate) async fn load(&mut self, namespace: &str) -> Result<Loaded> {
        let rows = postgres::load(&mut self.db, &self.sql.load, namespace)
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
        postgres::revisions(&mut self.db, &self.sql.revisions)
            .await
            .wrap_err(|| err!(RepoErr::QueryFailed, "live config revisions"))
    }

    pub(crate) async fn seed(&mut self, defaults: &[(String, Map<String, Value>)]) -> Result<()> {
        if defaults.iter().all(|(_, values)| values.is_empty()) {
            return Ok(());
        }
        postgres::seed(&mut self.db, &self.sql.seed, defaults)
            .await
            .wrap_err(|| err!(RepoErr::InsertFailed, "live config seed defaults"))
    }

    pub(crate) async fn trim_history(&mut self, keep_rows: u64) -> Result<u64> {
        // 配置里的数字很小，转不过去就用最大值（等于不删）
        let keep_rows = i64::try_from(keep_rows).unwrap_or(i64::MAX);
        postgres::trim_history(&mut self.db, &self.sql.trim_history, keep_rows)
            .await
            .wrap_err(|| err!(RepoErr::DeleteFailed, "live config trim history"))
    }

    /// 没有推送，一直等不到
    pub(crate) async fn recv(&mut self) -> Result<Signal> {
        std::future::pending().await
    }

    /// `Db` 是连接池自己的，由连接池关
    pub(crate) async fn close(&mut self) {}
}
