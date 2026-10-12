//! SQLite 连接池组件（sqlx），feature `sqlx` + `sqlite`

use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use hygiea_core::app::{AppErr, ConfigResource, ImmediateResourceComponent, async_trait};
use hygiea_core::{Result, ResultExt, err};

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

    fn connect_options(&self) -> Result<SqliteConnectOptions> {
        let mut options = SqliteConnectOptions::from_str(&self.url())
            .wrap_err(|| {
                err!(
                    AppErr::InvalidConfig,
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

fn parse_sqlite_option<T>(value: &str, name: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value.parse().wrap_err(|| {
        err!(
            AppErr::InvalidConfig,
            format!("invalid SQLite {name}: {value}")
        )
    })
}

// ---- pool -------------------------------------------------------------------

/// SQLite 连接池，`Deref` 到 sqlx 的 `SqlitePool`。用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct SqlxSqlitePool {
    inner: SqlitePool,
}

#[async_trait]
impl ConfigResource for SqlxSqlitePool {
    type Config = SqlxSqliteConfig;

    async fn from_config(config: &SqlxSqliteConfig) -> Result<Self> {
        tracing::info!("Connecting to SQLite database: {}", config.database);
        let connect_options = config.connect_options()?;
        let pool = if config.connect_lazy {
            config.pool_options().connect_lazy_with(connect_options)
        } else {
            config
                .pool_options()
                .connect_with(connect_options)
                .await
                .wrap_err(|| err!(AppErr::ConnectFailed, format!("sqlite {}", config.database)))?
        };
        tracing::info!("SQLite database connection established");
        Ok(Self { inner: pool })
    }

    /// 不再发新连接，等借出去的连接还回来（在途查询跑完）后全部关掉
    async fn close(&self) -> Result<()> {
        self.inner.close().await;
        tracing::info!("SQLite connection pool closed");
        Ok(())
    }
}

impl std::ops::Deref for SqlxSqlitePool {
    type Target = SqlitePool;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// SQLite 连接池组件（sqlx），按组件名放进 Resources 的是 [`SqlxSqlitePool`]。用法见 [`ImmediateResourceComponent`]
pub type SqlxSqliteComponent = ImmediateResourceComponent<SqlxSqlitePool>;

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
        let pool = SqlxSqlitePool::from_config(&config).await.unwrap();

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
    }

    #[tokio::test]
    async fn applies_common_sqlite_options() {
        let pool = SqlxSqlitePool::from_config(&SqlxSqliteConfig {
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

    /// `create_if_missing` 生效：开着时文件库不存在会被建出来
    #[tokio::test]
    async fn create_if_missing_creates_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("new.db");
        let config = SqlxSqliteConfig {
            database: path.to_string_lossy().to_string(),
            create_if_missing: true,
            ..Default::default()
        };
        SqlxSqlitePool::from_config(&config).await.unwrap();
        assert!(path.exists());
    }

    /// `create_if_missing` 关掉时，文件库不存在就连不上，也不会建文件
    #[tokio::test]
    async fn without_create_if_missing_missing_file_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("absent.db");
        let config = SqlxSqliteConfig {
            database: path.to_string_lossy().to_string(),
            create_if_missing: false,
            ..Default::default()
        };
        assert!(SqlxSqlitePool::from_config(&config).await.is_err());
        assert!(!path.exists());
    }
}
