//! SQLite 连接池组件（sqlx），feature `sqlx` + `sqlite`

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use hygiea_core::app::{
    BaseAppErr, CancellationToken, ImmediateComponent, Name, ResourceId, Resources, component,
};
use hygiea_core::{HyErr, ResultExt, err};

// ---- config -----------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlxSqliteConfig {
    /// SQLite database path. Use `:memory:` for an in-memory database.
    pub database: String,
    /// Additional SQLite URL query parameters, without the leading `?`.
    pub params: String,
    pub create_if_missing: bool,
    pub read_only: bool,
    pub foreign_keys: bool,
    pub shared_cache: bool,

    // ========== SQLite behavior ==========
    pub journal_mode: Option<String>,
    pub synchronous: Option<String>,
    pub busy_timeout_millis: Option<u64>,
    pub statement_cache_capacity: Option<usize>,

    // ========== connection pool ==========
    pub max_connections: Option<u32>,
    pub min_connections: Option<u32>,

    // ========== timeouts ==========
    pub acquire_timeout_secs: Option<u64>,
    pub idle_timeout_secs: Option<u64>,
    pub max_lifetime_secs: Option<u64>,

    // ========== pool behavior ==========
    pub test_before_acquire: bool,
    pub connect_lazy: bool,
}

impl Default for SqlxSqliteConfig {
    fn default() -> Self {
        Self {
            database: "hygiea.db".to_string(),
            params: String::new(),
            create_if_missing: true,
            read_only: false,
            foreign_keys: true,
            shared_cache: false,
            journal_mode: Some("wal".to_string()),
            synchronous: Some("full".to_string()),
            busy_timeout_millis: Some(5_000),
            statement_cache_capacity: Some(100),
            max_connections: Some(4),
            min_connections: Some(1),
            acquire_timeout_secs: Some(30),
            idle_timeout_secs: Some(600),
            max_lifetime_secs: Some(1800),
            test_before_acquire: true,
            connect_lazy: false,
        }
    }
}

impl SqlxSqliteConfig {
    pub fn url(&self) -> String {
        let base = if self.database == ":memory:" {
            "sqlite::memory:".to_string()
        } else {
            format!("sqlite://{}", self.database)
        };
        if self.params.is_empty() {
            base
        } else {
            format!("{base}?{}", self.params.trim_start_matches('?'))
        }
    }

    fn connect_options(&self) -> Result<SqliteConnectOptions, HyErr> {
        let mut options = SqliteConnectOptions::from_str(&self.url())
            .wrap_err(|| {
                err!(
                    BaseAppErr::InvalidConfig,
                    format!("invalid SQLite database URL: {}", self.url())
                )
            })?
            .create_if_missing(self.create_if_missing)
            .read_only(self.read_only)
            .foreign_keys(self.foreign_keys)
            .shared_cache(self.shared_cache);

        if let Some(value) = &self.journal_mode {
            options = options.journal_mode(parse_sqlite_option::<SqliteJournalMode>(
                value,
                "journal_mode",
            )?);
        }
        if let Some(value) = &self.synchronous {
            options = options.synchronous(parse_sqlite_option::<SqliteSynchronous>(
                value,
                "synchronous",
            )?);
        }
        if let Some(value) = self.busy_timeout_millis {
            options = options.busy_timeout(Duration::from_millis(value));
        }
        if let Some(value) = self.statement_cache_capacity {
            options = options.statement_cache_capacity(value);
        }
        Ok(options)
    }

    fn pool_options(&self) -> SqlitePoolOptions {
        let mut options = SqlitePoolOptions::new();
        if let Some(value) = self.max_connections {
            options = options.max_connections(value);
        }
        if let Some(value) = self.min_connections {
            options = options.min_connections(value);
        }
        if let Some(value) = self.acquire_timeout_secs {
            options = options.acquire_timeout(Duration::from_secs(value));
        }
        if let Some(value) = self.idle_timeout_secs {
            options = options.idle_timeout(Duration::from_secs(value));
        }
        if let Some(value) = self.max_lifetime_secs {
            options = options.max_lifetime(Duration::from_secs(value));
        }
        options.test_before_acquire(self.test_before_acquire)
    }
}

fn parse_sqlite_option<T>(value: &str, name: &str) -> Result<T, HyErr>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value.parse().wrap_err(|| {
        err!(
            BaseAppErr::InvalidConfig,
            format!("invalid SQLite {name}: {value}")
        )
    })
}

// ---- pool -------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SqlxSqlitePool {
    pub inner: SqlitePool,
    config: Arc<SqlxSqliteConfig>,
}

impl SqlxSqlitePool {
    pub fn new(inner: SqlitePool, config: Arc<SqlxSqliteConfig>) -> Self {
        Self { inner, config }
    }

    pub fn config(&self) -> &SqlxSqliteConfig {
        &self.config
    }

    pub async fn connect(config: SqlxSqliteConfig) -> Result<Self, HyErr> {
        let connect_options = config.connect_options()?;
        let pool = if config.connect_lazy {
            config.pool_options().connect_lazy_with(connect_options)
        } else {
            config
                .pool_options()
                .connect_with(connect_options)
                .await
                .wrap_err(|| {
                    err!(
                        BaseAppErr::ConnectFailed,
                        format!("sqlite {}", config.database)
                    )
                })?
        };
        Ok(Self::new(pool, Arc::new(config)))
    }
}

impl std::ops::Deref for SqlxSqlitePool {
    type Target = SqlitePool;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// 同一个组件可以用不同的名字注册多次（比如主库、从库），连接池按组件名放进 Resources，
/// 用 `get_named::<..>(name)` 取；匿名注册（名字是 `""`）的用 `get::<..>()` 取
pub struct SqlxSqliteComponent {
    name: Name,
    config: SqlxSqliteConfig,
    /// 启动后留一份连接池，关闭时在 `stop` 里关掉
    pool: Option<SqlxSqlitePool>,
}

#[component]
impl ImmediateComponent for SqlxSqliteComponent {
    type Config = SqlxSqliteConfig;

    fn build(name: Name, config: Self::Config) -> Self {
        Self {
            name,
            config,
            pool: None,
        }
    }

    /// 连接池按组件名放进 Resources，依赖它的组件声明 `ResourceId::named::<SqlxSqlitePool>(名字)`
    fn provides(&self) -> Vec<ResourceId> {
        vec![ResourceId::named::<SqlxSqlitePool>(self.name.clone())]
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, HyErr> {
        tracing::info!("Connecting to SQLite database: {}", self.config.database);
        let pool = SqlxSqlitePool::connect(self.config.clone()).await?;
        tracing::info!("SQLite database connection established");
        resources.insert_named(self.name.clone(), pool.clone());
        self.pool = Some(pool);
        Ok(None)
    }

    async fn stop(&mut self) -> Result<(), HyErr> {
        if let Some(pool) = self.pool.take() {
            // 不再发新连接，等借出去的连接还回来（在途查询跑完）后全部关掉
            pool.inner.close().await;
            tracing::info!("SQLite connection pool closed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_default() {
        assert_eq!(SqlxSqliteConfig::default().url(), "sqlite://hygiea.db");
    }

    #[test]
    fn build_url_memory_with_params() {
        let config = SqlxSqliteConfig {
            database: ":memory:".to_string(),
            params: "cache=shared".to_string(),
            ..Default::default()
        };
        assert_eq!(config.url(), "sqlite::memory:?cache=shared");
    }

    #[tokio::test]
    async fn connect_and_query_memory_database() {
        let config = SqlxSqliteConfig {
            database: ":memory:".to_string(),
            max_connections: Some(1),
            ..Default::default()
        };
        let pool = SqlxSqlitePool::connect(config).await.unwrap();

        sqlx::query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .execute(&*pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO items (name) VALUES (?)")
            .bind("hygiea")
            .execute(&*pool)
            .await
            .unwrap();

        let (name,): (String,) = sqlx::query_as("SELECT name FROM items WHERE id = ?")
            .bind(1_i64)
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(name, "hygiea");
        assert_eq!(pool.config().database, ":memory:");
    }

    #[tokio::test]
    async fn applies_common_sqlite_options() {
        let pool = SqlxSqlitePool::connect(SqlxSqliteConfig {
            database: ":memory:".to_string(),
            max_connections: Some(1),
            journal_mode: Some("memory".to_string()),
            synchronous: Some("normal".to_string()),
            busy_timeout_millis: Some(2_500),
            statement_cache_capacity: Some(64),
            ..Default::default()
        })
        .await
        .unwrap();

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&*pool)
            .await
            .unwrap();
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&*pool)
            .await
            .unwrap();
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(journal_mode, "memory");
        assert_eq!(synchronous, 1);
        assert_eq!(busy_timeout, 2_500);
    }

    #[test]
    fn rejects_invalid_sqlite_mode() {
        let config = SqlxSqliteConfig {
            journal_mode: Some("not-a-mode".to_string()),
            ..Default::default()
        };
        assert!(config.connect_options().is_err());
    }

    #[test]
    fn build_url_strips_leading_question_mark_from_params() {
        let config = SqlxSqliteConfig {
            database: ":memory:".to_string(),
            params: "?a=b&c=d".to_string(),
            ..Default::default()
        };
        assert_eq!(config.url(), "sqlite::memory:?a=b&c=d");
    }

    #[test]
    fn build_url_from_file_path() {
        let config = SqlxSqliteConfig {
            database: "/var/lib/app/data.db".to_string(),
            params: String::new(),
            ..Default::default()
        };
        assert_eq!(config.url(), "sqlite:///var/lib/app/data.db");
    }
}
