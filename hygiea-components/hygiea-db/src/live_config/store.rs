//! worker 和各驱动之间的接口：worker 只认 [`Store`]，它按驱动分发给各驱动的实现（现在只有 `sqlx/`）。
//! 方法的参数、返回值里没有驱动的类型，错误已经转成 [`RepoErr`](crate::RepoErr)

use serde_json::{Map, Value};

use hygiea_core::Result;

use super::Backend;
use super::schema::Tables;
#[cfg(sqlx_live_config)]
use super::sqlx::SqlxStore;
use super::worker::{Loaded, Signal};
#[cfg(sqlx_live_config)]
use crate::PoolConfig;

/// 一个连接池的动态配置用的连接和在上面做的事
pub(crate) enum Store {
    #[cfg(sqlx_live_config)]
    Sqlx(SqlxStore),
}

/// 每个方法都是原样转给驱动的实现
macro_rules! each {
    ($store:expr, $s:ident => $e:expr) => {
        match $store {
            #[cfg(sqlx_live_config)]
            Store::Sqlx($s) => $e,
        }
    };
}

impl Store {
    /// `options` 是主池的连接参数，在独立小池上建（见 `sqlx/mod.rs`）
    #[cfg(all(sqlx_live_config, sqlx_pg))]
    pub(crate) fn postgres(
        options: sqlx::postgres::PgConnectOptions,
        main: &PoolConfig,
        tables: Tables,
    ) -> Self {
        Self::Sqlx(SqlxStore::postgres(options, main, tables))
    }

    #[cfg(all(sqlx_live_config, sqlx_mysql))]
    pub(crate) fn mysql(
        options: sqlx::mysql::MySqlConnectOptions,
        main: &PoolConfig,
        tables: Tables,
    ) -> Self {
        Self::Sqlx(SqlxStore::mysql(options, main, tables))
    }

    #[cfg(all(sqlx_live_config, sqlx_sqlite))]
    pub(crate) fn sqlite(
        options: sqlx::sqlite::SqliteConnectOptions,
        main: &PoolConfig,
        tables: Tables,
    ) -> Self {
        Self::Sqlx(SqlxStore::sqlite(options, main, tables))
    }

    pub(crate) fn backend(&self) -> Backend {
        each!(self, s => s.backend())
    }

    /// 有没有推送：没有的只靠轮询，轮询间隔默认短一些
    pub(crate) fn has_push(&self) -> bool {
        each!(self, s => s.has_push())
    }

    /// 配置表名，日志用
    pub(crate) fn table(&self) -> String {
        each!(self, s => s.table())
    }

    pub(crate) fn is_open(&self) -> bool {
        each!(self, s => s.is_open())
    }

    /// 连上；`ensure_tables` 时检查表和函数、触发器，没有就建（规则见 `schema.rs`）
    pub(crate) async fn open(&mut self, ensure_tables: bool) -> Result<()> {
        each!(self, s => s.open(ensure_tables).await)
    }

    pub(crate) async fn load(&mut self, namespace: &str) -> Result<Loaded> {
        each!(self, s => s.load(namespace).await)
    }

    pub(crate) async fn revisions(&mut self) -> Result<Vec<(String, i64)>> {
        each!(self, s => s.revisions().await)
    }

    pub(crate) async fn seed(&mut self, defaults: &[(String, Map<String, Value>)]) -> Result<()> {
        each!(self, s => s.seed(defaults).await)
    }

    pub(crate) async fn trim_history(&mut self, keep_rows: u64) -> Result<u64> {
        each!(self, s => s.trim_history(keep_rows).await)
    }

    /// 等下一个变更信号；没有推送的一直等不到。要能被取消（worker 在 `select!` 里等它）
    pub(crate) async fn recv(&mut self) -> Result<Signal> {
        each!(self, s => s.recv().await)
    }

    pub(crate) async fn close(&mut self) {
        each!(self, s => s.close().await)
    }
}
