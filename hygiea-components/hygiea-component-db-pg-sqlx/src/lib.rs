use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use hygiea_core::app::{Component, Resources, async_trait};

// ---- config -----------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
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

    // ========== 分布式锁 ==========
    #[cfg(feature = "distributed-lock")]
    pub lock_table: String,
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
            #[cfg(feature = "distributed-lock")]
            lock_table: "hygiea_distributed_locks".to_string(),
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
    #[cfg_attr(not(feature = "distributed-lock"), allow(dead_code))]
    config: Arc<SqlxPgConfig>,
}

impl SqlxPgPool {
    pub fn new(inner: PgPool, config: Arc<SqlxPgConfig>) -> Self {
        Self { inner, config }
    }

    pub async fn connect(config: SqlxPgConfig) -> Result<Self, anyhow::Error> {
        let pool = if config.connect_lazy {
            config.pool_options().connect_lazy(&config.url())
        } else {
            config.pool_options().connect(&config.url()).await
        }
        .context("Failed to connect to database")?;
        let config = Arc::new(config);
        #[cfg(feature = "distributed-lock")]
        distributed_lock::ensure_lock_table(&pool, &config.lock_table).await?;
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

pub struct SqlxPgComponent {
    config: SqlxPgConfig,
}

#[async_trait]
impl Component for SqlxPgComponent {
    type Config = SqlxPgConfig;

    fn build(_name: &'static str, config: Self::Config) -> Self {
        Self { config }
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, anyhow::Error> {
        tracing::info!(
            "Connecting to database: {}@{}:{}/{}",
            self.config.username,
            self.config.host,
            self.config.port,
            self.config.database
        );
        let pool = SqlxPgPool::connect(self.config.clone()).await?;
        tracing::info!("Database connection established");
        resources.insert(pool);
        Ok(None)
    }
}

// ---- distributed lock -------------------------------------------------------

#[cfg(feature = "distributed-lock")]
pub use hygiea_core::{DistributedKey, DistributedLock};

#[cfg(feature = "distributed-lock")]
mod distributed_lock {
    use super::*;
    use std::future::Future;

    pub(super) async fn ensure_lock_table(pool: &PgPool, table: &str) -> Result<(), anyhow::Error> {
        // key is VARCHAR(255): sufficient for any practical lock key (UUIDs, namespaced paths, etc.)
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (key VARCHAR(255) PRIMARY KEY)",
            table
        );
        sqlx::query(&sql)
            .execute(pool)
            .await
            .context("Failed to create lock table")?;
        tracing::info!("{} table ready", table);
        Ok(())
    }

    #[async_trait]
    impl DistributedLock for SqlxPgPool {
        type Error = sqlx::Error;

        async fn lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<T, Self::Error>
        where
            K: Into<DistributedKey> + Send,
            F: FnOnce() -> Fut + Send,
            Fut: Future<Output = Result<T, Self::Error>> + Send,
            T: Send + 'static,
        {
            let mut tx = self.inner.begin().await?;
            acquire_lock(&mut tx, key.into(), &self.config.lock_table).await?;
            let result = f().await;
            tx.rollback().await?;
            result
        }

        async fn try_lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<Option<T>, Self::Error>
        where
            K: Into<DistributedKey> + Send,
            F: FnOnce() -> Fut + Send,
            Fut: Future<Output = Result<T, Self::Error>> + Send,
            T: Send + 'static,
        {
            let mut tx = self.inner.begin().await?;
            if !try_acquire_lock(&mut tx, key.into(), &self.config.lock_table).await? {
                return Ok(None);
            }
            let result = f().await;
            tx.rollback().await?;
            result.map(Some)
        }
    }

    async fn acquire_lock(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: DistributedKey,
        lock_table: &str,
    ) -> Result<(), sqlx::Error> {
        match key {
            DistributedKey::Advisory(k) => {
                sqlx::query("SELECT pg_advisory_xact_lock($1)")
                    .bind(k)
                    .execute(&mut **tx)
                    .await?;
            }
            DistributedKey::Advisory2(a, b) => {
                sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
                    .bind(a)
                    .bind(b)
                    .execute(&mut **tx)
                    .await?;
            }
            DistributedKey::Named(k) => {
                sqlx::query(&format!(
                    "INSERT INTO {} (key) VALUES ($1) ON CONFLICT DO NOTHING",
                    lock_table
                ))
                .bind(&k)
                .execute(&mut **tx)
                .await?;
                sqlx::query(&format!(
                    "SELECT key FROM {} WHERE key = $1 FOR UPDATE",
                    lock_table
                ))
                .bind(&k)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    async fn try_acquire_lock(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: DistributedKey,
        lock_table: &str,
    ) -> Result<bool, sqlx::Error> {
        match key {
            DistributedKey::Advisory(k) => {
                let (ok,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_xact_lock($1)")
                    .bind(k)
                    .fetch_one(&mut **tx)
                    .await?;
                Ok(ok)
            }
            DistributedKey::Advisory2(a, b) => {
                let (ok,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_xact_lock($1, $2)")
                    .bind(a)
                    .bind(b)
                    .fetch_one(&mut **tx)
                    .await?;
                Ok(ok)
            }
            DistributedKey::Named(k) => {
                sqlx::query(&format!(
                    "INSERT INTO {} (key) VALUES ($1) ON CONFLICT DO NOTHING",
                    lock_table
                ))
                .bind(&k)
                .execute(&mut **tx)
                .await?;
                let result = sqlx::query(&format!(
                    "SELECT key FROM {} WHERE key = $1 FOR UPDATE NOWAIT",
                    lock_table
                ))
                .bind(&k)
                .execute(&mut **tx)
                .await;
                match result {
                    Ok(_) => Ok(true),
                    Err(sqlx::Error::Database(e))
                        if e.downcast_ref::<sqlx::postgres::PgDatabaseError>().code()
                            == "55P03" =>
                    {
                        Ok(false)
                    }
                    Err(e) => Err(e),
                }
            }
        }
    }
}
