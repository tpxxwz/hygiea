use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use hygiea_core::app::{Component, Resources, async_trait};

// ---- config -----------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
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

    fn connect_options(&self) -> Result<SqliteConnectOptions, anyhow::Error> {
        let mut options = SqliteConnectOptions::from_str(&self.url())
            .context("Invalid SQLite database URL")?
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

fn parse_sqlite_option<T>(value: &str, name: &str) -> Result<T, anyhow::Error>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value
        .parse()
        .map_err(anyhow::Error::new)
        .with_context(|| format!("Invalid SQLite {name}: {value}"))
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

    pub async fn connect(config: SqlxSqliteConfig) -> Result<Self, anyhow::Error> {
        let connect_options = config.connect_options()?;
        let pool = if config.connect_lazy {
            config.pool_options().connect_lazy_with(connect_options)
        } else {
            config
                .pool_options()
                .connect_with(connect_options)
                .await
                .context("Failed to connect to SQLite database")?
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

pub struct SqlxSqliteComponent {
    config: SqlxSqliteConfig,
}

#[async_trait]
impl Component for SqlxSqliteComponent {
    type Config = SqlxSqliteConfig;

    fn build(_name: &'static str, config: Self::Config) -> Self {
        Self { config }
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, anyhow::Error> {
        tracing::info!("Connecting to SQLite database: {}", self.config.database);
        let pool = SqlxSqlitePool::connect(self.config.clone()).await?;
        tracing::info!("SQLite database connection established");
        resources.insert(pool);
        Ok(None)
    }
}

// ---- process-local lock -----------------------------------------------------

/// SQLite lock support is process-local and keyed. It is intended for deployments
/// where exactly one application process accesses the configured database file.
#[cfg(feature = "distributed-lock")]
pub use hygiea_core::sync::{DistributedKey, DistributedLock};

#[cfg(feature = "distributed-lock")]
mod distributed_lock {
    use super::*;
    use std::collections::HashMap;
    use std::future::Future;
    use std::sync::{Mutex, OnceLock, Weak};
    use tokio::sync::Mutex as AsyncMutex;

    type LockRegistry = Mutex<HashMap<String, Weak<AsyncMutex<()>>>>;

    static LOCKS: OnceLock<LockRegistry> = OnceLock::new();

    fn lock_id(database: &str, key: DistributedKey) -> String {
        let key = match key {
            DistributedKey::Advisory(value) => format!("advisory:{value}"),
            DistributedKey::Advisory2(first, second) => {
                format!("advisory2:{first}:{second}")
            }
            DistributedKey::Named(value) => format!("named:{}:{value}", value.len()),
        };
        format!("database:{}:{database}:{key}", database.len())
    }

    fn keyed_lock(database: &str, key: DistributedKey) -> Arc<AsyncMutex<()>> {
        let id = lock_id(database, key);
        let mut locks = LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lock) = locks.get(&id).and_then(Weak::upgrade) {
            return lock;
        }
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(id, Arc::downgrade(&lock));
        lock
    }

    #[async_trait]
    impl DistributedLock for SqlxSqlitePool {
        type Error = sqlx::Error;

        async fn lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<T, Self::Error>
        where
            K: Into<DistributedKey> + Send,
            F: FnOnce() -> Fut + Send,
            Fut: Future<Output = Result<T, Self::Error>> + Send,
            T: Send + 'static,
        {
            let lock = keyed_lock(&self.config.database, key.into());
            let _guard = lock.lock_owned().await;
            f().await
        }

        async fn try_lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<Option<T>, Self::Error>
        where
            K: Into<DistributedKey> + Send,
            F: FnOnce() -> Fut + Send,
            Fut: Future<Output = Result<T, Self::Error>> + Send,
            T: Send + 'static,
        {
            let lock = keyed_lock(&self.config.database, key.into());
            let Ok(_guard) = lock.try_lock_owned() else {
                return Ok(None);
            };
            f().await.map(Some)
        }
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
            shared_cache: true,
            max_connections: Some(1),
            ..Default::default()
        };
        let pool = SqlxSqlitePool::connect(config).await.unwrap();

        sqlx::query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .execute(&pool.inner)
            .await
            .unwrap();
        sqlx::query("INSERT INTO items (name) VALUES (?)")
            .bind("hygiea")
            .execute(&pool.inner)
            .await
            .unwrap();

        let (name,): (String,) = sqlx::query_as("SELECT name FROM items WHERE id = ?")
            .bind(1_i64)
            .fetch_one(&pool.inner)
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
            .fetch_one(&pool.inner)
            .await
            .unwrap();
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&pool.inner)
            .await
            .unwrap();
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&pool.inner)
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

    #[cfg(feature = "distributed-lock")]
    #[tokio::test]
    async fn process_local_locks_are_keyed() {
        use hygiea_core::sync::DistributedLock;
        use tokio::sync::Notify;

        let pool = SqlxSqlitePool::connect(SqlxSqliteConfig {
            database: ":memory:".to_string(),
            max_connections: Some(1),
            ..Default::default()
        })
        .await
        .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let task = {
            let pool = pool.clone();
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                pool.lock("same-key", || async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok::<_, sqlx::Error>(())
                })
                .await
                .unwrap();
            })
        };

        entered.notified().await;
        assert!(
            pool.try_lock("same-key", || async { Ok::<_, sqlx::Error>(()) })
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            pool.try_lock("different-key", || async { Ok::<_, sqlx::Error>(()) })
                .await
                .unwrap()
                .is_some()
        );

        release.notify_one();
        task.await.unwrap();
        assert!(
            pool.try_lock("same-key", || async { Ok::<_, sqlx::Error>(()) })
                .await
                .unwrap()
                .is_some()
        );
    }
}
