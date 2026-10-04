//! PostgreSQL 连接池组件（sqlx），feature `sqlx` + `postgres`

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use hygiea_core::app::{
    BaseAppErr, CancellationToken, ImmediateComponent, Name, ResourceId, Resources, component,
};
use hygiea_core::{Result, ResultExt, err};

// ---- config -----------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlxPgConfig {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
    pub params: String,
    pub schema_search_path: String,

    // ========== 连接池 ==========
    pub max_connections: Option<u32>,
    pub min_connections: Option<u32>,

    // ========== 超时 ==========
    pub acquire_timeout_secs: Option<u64>,
    pub idle_timeout_secs: Option<u64>,
    pub max_lifetime_secs: Option<u64>,

    // ========== 连接池行为 ==========
    pub test_before_acquire: bool,
    pub connect_lazy: bool,
}

impl Default for SqlxPgConfig {
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
            acquire_timeout_secs: Some(30),
            idle_timeout_secs: Some(600),
            max_lifetime_secs: Some(1800),
            test_before_acquire: true,
            connect_lazy: false,
        }
    }
}

impl SqlxPgConfig {
    pub fn url(&self) -> String {
        let mut parts = vec![];
        if !self.schema_search_path.is_empty() {
            parts.push(format!(
                "options=-c search_path={}",
                self.schema_search_path
            ));
        }
        if !self.params.is_empty() {
            parts.push(self.params.clone());
        }
        let query = if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        };
        format!(
            "{}://{}:{}@{}:{}/{}{}",
            self.scheme, self.username, self.password, self.host, self.port, self.database, query,
        )
    }

    fn pool_options(&self) -> PgPoolOptions {
        let mut opts = PgPoolOptions::new();
        if let Some(v) = self.max_connections {
            opts = opts.max_connections(v);
        }
        if let Some(v) = self.min_connections {
            opts = opts.min_connections(v);
        }
        if let Some(v) = self.acquire_timeout_secs {
            opts = opts.acquire_timeout(Duration::from_secs(v));
        }
        if let Some(v) = self.idle_timeout_secs {
            opts = opts.idle_timeout(Duration::from_secs(v));
        }
        if let Some(v) = self.max_lifetime_secs {
            opts = opts.max_lifetime(Duration::from_secs(v));
        }
        opts.test_before_acquire(self.test_before_acquire)
    }
}

// ---- pool -------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SqlxPgPool {
    pub inner: PgPool,
    config: Arc<SqlxPgConfig>,
}

impl SqlxPgPool {
    pub fn new(inner: PgPool, config: Arc<SqlxPgConfig>) -> Self {
        Self { inner, config }
    }

    /// 连接池用的配置
    pub fn config(&self) -> &SqlxPgConfig {
        &self.config
    }

    pub async fn connect(config: SqlxPgConfig) -> Result<Self> {
        let pool = if config.connect_lazy {
            config.pool_options().connect_lazy(&config.url())
        } else {
            config.pool_options().connect(&config.url()).await
        }
        .wrap_err(|| {
            err!(
                BaseAppErr::ConnectFailed,
                format!(
                    "postgres {}:{}/{}",
                    config.host, config.port, config.database
                )
            )
        })?;
        let config = Arc::new(config);
        Ok(Self::new(pool, config))
    }
}

impl std::ops::Deref for SqlxPgPool {
    type Target = PgPool;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// 同一个组件可以用不同的名字注册多次（比如主库、从库），连接池按组件名放进 Resources，
/// 用 `get_named::<..>(name)` 取；匿名注册（名字是 `""`）的用 `get::<..>()` 取
pub struct SqlxPgComponent {
    name: Name,
    config: SqlxPgConfig,
    /// 启动后留一份连接池，关闭时在 `stop` 里关掉
    pool: Option<SqlxPgPool>,
}

#[component]
impl ImmediateComponent for SqlxPgComponent {
    type Config = SqlxPgConfig;

    fn build(name: Name, config: Self::Config) -> Self {
        Self {
            name,
            config,
            pool: None,
        }
    }

    /// 连接池按组件名放进 Resources，依赖它的组件声明 `ResourceId::named::<SqlxPgPool>(名字)`
    fn provides(&self) -> Vec<ResourceId> {
        vec![ResourceId::named::<SqlxPgPool>(self.name.clone())]
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
        let pool = SqlxPgPool::connect(self.config.clone()).await?;
        tracing::info!("Database connection established");
        resources.insert_named(self.name.clone(), pool.clone());
        self.pool = Some(pool);
        Ok(None)
    }

    async fn stop(&mut self) -> Result<()> {
        if let Some(pool) = self.pool.take() {
            // 不再发新连接，等借出去的连接还回来（在途查询跑完）后全部关掉
            pool.inner.close().await;
            tracing::info!("Database connection pool closed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_default() {
        let config = SqlxPgConfig::default();
        assert_eq!(
            config.url(),
            "postgres://postgres:postgres@localhost:5432/postgres"
        );
    }

    #[test]
    fn build_url_only_schema_search_path() {
        let config = SqlxPgConfig {
            schema_search_path: "public,custom".to_string(),
            ..Default::default()
        };
        let url = config.url();
        assert!(url.contains("?options=-c search_path=public,custom"));
    }

    #[test]
    fn build_url_only_params() {
        let config = SqlxPgConfig {
            params: "sslmode=require".to_string(),
            ..Default::default()
        };
        let url = config.url();
        assert!(url.contains("?sslmode=require"));
        assert!(!url.contains("options=-c"));
    }

    #[test]
    fn build_url_with_both_schema_and_params() {
        let config = SqlxPgConfig {
            schema_search_path: "public".to_string(),
            params: "sslmode=require".to_string(),
            ..Default::default()
        };
        let url = config.url();
        assert!(url.contains("?options=-c search_path=public&sslmode=require"));
    }

    #[test]
    fn config_default_values() {
        let config = SqlxPgConfig::default();
        assert_eq!(config.acquire_timeout_secs, Some(30));
        assert_eq!(config.idle_timeout_secs, Some(600));
        assert!(!config.connect_lazy);
    }

    #[tokio::test]
    async fn connect_lazy_to_nonexistent_host_succeeds() {
        // 即使是连不到的主机，connect_lazy 也应该成功
        let config = SqlxPgConfig {
            host: "nonexistent-host-xyz-12345.invalid".to_string(),
            connect_lazy: true,
            ..Default::default()
        };
        // 不应该报错，因为是 lazy connect
        let result = SqlxPgPool::connect(config).await;
        assert!(result.is_ok());
    }
}
