//! 连接池组件，一种数据库一个文件，再导出到 crate 根。各数据库共用的：日志配置 [`SeaOrmLogConfig`]、池参数

#[cfg(any(sqlx_pg, sqlx_mysql, sqlx_sqlite))]
use std::time::Duration;

#[cfg(any(sqlx_pg, sqlx_mysql, sqlx_sqlite))]
use sea_orm::ConnectOptions;
use serde::Deserialize;

#[cfg(any(sqlx_pg, sqlx_mysql, sqlx_sqlite))]
use crate::config::{PoolConfig, PoolSize};

#[cfg(feature = "mysql")]
pub(crate) mod mysql;
#[cfg(feature = "postgres")]
pub(crate) mod postgres;
#[cfg(feature = "sqlite")]
pub(crate) mod sqlite;

/// SeaORM 打的 SQL 日志，配置文件里是 `[db.log]`。级别是 trace / debug / info / warn / error / off
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeaOrmLogConfig {
    /// 每条 SQL 都打
    pub enabled: bool,
    pub level: String,
    /// 慢 SQL 单独打的级别，`off` 不打
    pub slow_statements_level: String,
    pub slow_statements_threshold_secs: u64,
}

impl Default for SeaOrmLogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: "info".to_string(),
            slow_statements_level: "off".to_string(),
            slow_statements_threshold_secs: 1,
        }
    }
}

/// 连接 URL 加上池参数（不写的用 `size` 给的这个数据库的默认值）和日志设置
#[cfg(any(sqlx_pg, sqlx_mysql, sqlx_sqlite))]
pub(crate) fn connect_options(
    url: String,
    pool: &PoolConfig,
    log: &SeaOrmLogConfig,
    size: PoolSize,
) -> ConnectOptions {
    let mut opt = ConnectOptions::new(url);
    opt.max_connections(pool.max_connections(size))
        .min_connections(pool.min_connections(size))
        .acquire_timeout(Duration::from_secs(pool.acquire_timeout_secs()))
        .idle_timeout(Duration::from_secs(pool.idle_timeout_secs()))
        .max_lifetime(Duration::from_secs(pool.max_lifetime_secs()))
        .test_before_acquire(pool.test_before_acquire)
        .connect_lazy(pool.connect_lazy)
        .sqlx_logging(log.enabled)
        .sqlx_logging_level(parse_log_level(&log.level))
        .sqlx_slow_statements_logging_settings(
            parse_log_level(&log.slow_statements_level),
            Duration::from_secs(log.slow_statements_threshold_secs),
        );
    opt
}

#[cfg(any(sqlx_pg, sqlx_mysql, sqlx_sqlite))]
fn parse_log_level(s: &str) -> log::LevelFilter {
    match s.to_lowercase().as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "info" => log::LevelFilter::Info,
        "warn" => log::LevelFilter::Warn,
        "error" => log::LevelFilter::Error,
        "off" => log::LevelFilter::Off,
        other => panic!(
            "Invalid log level '{}'. Expected: trace, debug, info, warn, error, off",
            other
        ),
    }
}
