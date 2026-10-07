//! PostgreSQL 连接组件（SeaORM），feature `seaorm` + `postgres`

use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use serde::Deserialize;

use hygiea_core::app::{
    AppErr, CancellationToken, ImmediateComponent, Name, ResourceId, Resources, component,
};
use hygiea_core::{Result, ResultExt, err};

// ---- config -----------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeaOrmPgConfig {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
    /// Optional URL parameters (e.g., "sslmode=require")
    pub params: String,
    /// Schema search path (PostgreSQL only, empty = use database default)
    pub schema_search_path: String,

    // ========== 连接池 ==========
    pub max_connections: Option<u32>,
    pub min_connections: Option<u32>,

    // ========== 超时 ==========
    pub connect_timeout_secs: Option<u64>,
    pub acquire_timeout_secs: Option<u64>,
    pub idle_timeout_secs: Option<u64>,
    pub max_lifetime_secs: Option<u64>,

    // ========== 连接池行为 ==========
    pub test_before_acquire: bool,
    pub connect_lazy: bool,

    // ========== 日志 ==========
    pub sqlx_logging: bool,
    pub sqlx_logging_level: String,
    pub sqlx_slow_statements_logging_level: String,
    pub sqlx_slow_statements_threshold_secs: u64,
}

impl Default for SeaOrmPgConfig {
    fn default() -> Self {
        Self {
            scheme: "postgres".to_string(),
            host: "localhost".to_string(),
            port: 5432,
            username: "postgres".to_string(),
            password: "postgres".to_string(),
            database: "postgres".to_string(),
            params: String::new(),
            schema_search_path: String::new(),
            max_connections: Some(10),
            min_connections: Some(0),
            connect_timeout_secs: Some(30),
            acquire_timeout_secs: Some(30),
            idle_timeout_secs: Some(600),
            max_lifetime_secs: Some(1800),
            test_before_acquire: true,
            connect_lazy: false,
            sqlx_logging: true,
            sqlx_logging_level: "info".to_string(),
            sqlx_slow_statements_logging_level: "off".to_string(),
            sqlx_slow_statements_threshold_secs: 1,
        }
    }
}

impl SeaOrmPgConfig {
    pub fn url(&self) -> String {
        let params_part = if self.params.is_empty() { "" } else { "?" };
        format!(
            "{}://{}:{}@{}:{}/{}{}{}",
            self.scheme,
            self.username,
            self.password,
            self.host,
            self.port,
            self.database,
            params_part,
            self.params
        )
    }

    fn connect_options(&self) -> ConnectOptions {
        let mut opt = ConnectOptions::new(self.url());
        if !self.schema_search_path.is_empty() {
            opt.set_schema_search_path(&self.schema_search_path);
        }
        if let Some(v) = self.max_connections {
            opt.max_connections(v);
        }
        if let Some(v) = self.min_connections {
            opt.min_connections(v);
        }
        if let Some(v) = self.connect_timeout_secs {
            opt.connect_timeout(Duration::from_secs(v));
        }
        if let Some(v) = self.acquire_timeout_secs {
            opt.acquire_timeout(Duration::from_secs(v));
        }
        if let Some(v) = self.idle_timeout_secs {
            opt.idle_timeout(Duration::from_secs(v));
        }
        if let Some(v) = self.max_lifetime_secs {
            opt.max_lifetime(Duration::from_secs(v));
        }
        opt.test_before_acquire(self.test_before_acquire);
        opt.connect_lazy(self.connect_lazy);
        opt.sqlx_logging(self.sqlx_logging);
        opt.sqlx_logging_level(parse_log_level(&self.sqlx_logging_level));
        opt.sqlx_slow_statements_logging_settings(
            parse_log_level(&self.sqlx_slow_statements_logging_level),
            Duration::from_secs(self.sqlx_slow_statements_threshold_secs),
        );
        opt
    }
}

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

// ---- pool -------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SeaOrmPgPool {
    pub inner: DatabaseConnection,
    config: Arc<SeaOrmPgConfig>,
}

impl SeaOrmPgPool {
    pub fn new(inner: DatabaseConnection, config: Arc<SeaOrmPgConfig>) -> Self {
        Self { inner, config }
    }

    /// 连接池用的配置
    pub fn config(&self) -> &SeaOrmPgConfig {
        &self.config
    }

    pub async fn connect(config: SeaOrmPgConfig) -> Result<Self> {
        let conn: DatabaseConnection =
            Database::connect(config.connect_options())
                .await
                .wrap_err(|| {
                    err!(
                        AppErr::ConnectFailed,
                        format!(
                            "postgres {}:{}/{}",
                            config.host, config.port, config.database
                        )
                    )
                })?;
        let config = Arc::new(config);
        Ok(Self::new(conn, config))
    }
}

impl std::ops::Deref for SeaOrmPgPool {
    type Target = DatabaseConnection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// 同一个组件可以用不同的名字注册多次（比如主库、从库），连接池按组件名放进 Resources，
/// 用 `get_named::<..>(name)` 取；匿名注册（名字是 `""`）的用 `get::<..>()` 取
pub struct SeaOrmPgComponent {
    name: Name,
    config: SeaOrmPgConfig,
    /// 启动后留一份连接池，关闭时在 `stop` 里关掉
    pool: Option<SeaOrmPgPool>,
}

#[component]
impl ImmediateComponent for SeaOrmPgComponent {
    type Config = SeaOrmPgConfig;

    fn build(name: Name, config: Self::Config) -> Self {
        Self {
            name,
            config,
            pool: None,
        }
    }

    /// 连接池按组件名放进 Resources，依赖它的组件声明 `ResourceId::named::<SeaOrmPgPool>(名字)`
    fn provides(&self) -> Vec<ResourceId> {
        vec![ResourceId::named::<SeaOrmPgPool>(self.name.clone())]
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>> {
        tracing::info!(
            "Connecting to database: {}@{}:{}/{}",
            self.config.username,
            self.config.host,
            self.config.port,
            self.config.database
        );
        let pool = SeaOrmPgPool::connect(self.config.clone()).await?;
        tracing::info!("Database connection established");
        resources.insert_named(self.name.clone(), pool.clone());
        self.pool = Some(pool);
        Ok(None)
    }

    async fn stop(&mut self) -> Result<()> {
        if let Some(pool) = self.pool.take() {
            // 等在途查询跑完后关掉连接池
            pool.inner
                .close()
                .await
                .wrap_err(|| err!(AppErr::StopFailed, "close database connection"))?;
            tracing::info!("Database connection pool closed");
        }
        Ok(())
    }
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_default_no_params() {
        let config = SeaOrmPgConfig::default();
        let url = config.url();
        assert_eq!(url, "postgres://postgres:postgres@localhost:5432/postgres");
        assert!(!url.contains('?'));
    }

    #[test]
    fn build_url_with_params() {
        let config = SeaOrmPgConfig {
            params: "sslmode=require".to_string(),
            ..SeaOrmPgConfig::default()
        };
        assert!(config.url().ends_with("?sslmode=require"));
    }
}
