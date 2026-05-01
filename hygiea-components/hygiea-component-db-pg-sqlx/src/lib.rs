use std::future::Future;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use sqlx::PgPool;
use sqlx::postgres::{PgDatabaseError, PgPoolOptions};

use hygiea_core::app::{Component, Resources, async_trait};

pub use hygiea_core::{DistributedKey, DistributedLock};

// ---- config -----------------------------------------------------------------

#[derive(Deserialize, Clone)]
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
    pub lock_schema: String,

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
            lock_schema: String::new(),
            max_connections: Some(10),
            min_connections: Some(0),
            connect_timeout_secs: Some(30),
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

    pub fn effective_lock_schema(&self) -> &str {
        if !self.lock_schema.is_empty() {
            return &self.lock_schema;
        }
        self.schema_search_path
            .split(',')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("public")
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

        let pool = build_pool(&self.config)
            .await
            .context("Failed to connect to database")?;

        tracing::info!("Database connection established");

        resources.insert(SqlxPgPool::new(
            pool,
            self.config.effective_lock_schema().to_string(),
        ));

        Ok(None)
    }
}

// ---- SqlxPgPool -------------------------------------------------------------

pub struct SqlxPgPool {
    pub inner: PgPool,
    pub lock_schema: String,
}

impl SqlxPgPool {
    pub fn new(inner: PgPool, lock_schema: String) -> Self {
        Self { inner, lock_schema }
    }
}

impl std::ops::Deref for SqlxPgPool {
    type Target = PgPool;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[async_trait]
impl DistributedLock for SqlxPgPool {
    type Error = sqlx::Error;

    async fn with_lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<T, Self::Error>
    where
        K: Into<DistributedKey> + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<T, Self::Error>> + Send,
        T: Send + 'static,
    {
        let mut tx = self.inner.begin().await?;
        acquire_lock(&mut tx, key.into(), &self.lock_schema).await?;
        let result = f().await;
        tx.rollback().await?;
        result
    }

    async fn try_with_lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<Option<T>, Self::Error>
    where
        K: Into<DistributedKey> + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<T, Self::Error>> + Send,
        T: Send + 'static,
    {
        let mut tx = self.inner.begin().await?;
        if !try_acquire_lock(&mut tx, key.into(), &self.lock_schema).await? {
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
    lock_schema: &str,
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
            let table = format!("{}.distributed_locks", lock_schema);
            sqlx::query(&format!(
                "INSERT INTO {} (key) VALUES ($1) ON CONFLICT DO NOTHING",
                table
            ))
            .bind(&k)
            .execute(&mut **tx)
            .await?;
            sqlx::query(&format!(
                "SELECT key FROM {} WHERE key = $1 FOR UPDATE",
                table
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
    lock_schema: &str,
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
            let table = format!("{}.distributed_locks", lock_schema);
            sqlx::query(&format!(
                "INSERT INTO {} (key) VALUES ($1) ON CONFLICT DO NOTHING",
                table
            ))
            .bind(&k)
            .execute(&mut **tx)
            .await?;
            let result = sqlx::query(&format!(
                "SELECT key FROM {} WHERE key = $1 FOR UPDATE NOWAIT",
                table
            ))
            .bind(&k)
            .execute(&mut **tx)
            .await;
            match result {
                Ok(_) => Ok(true),
                Err(sqlx::Error::Database(e))
                    if e.downcast_ref::<PgDatabaseError>().code() == "55P03" =>
                {
                    Ok(false)
                }
                Err(e) => Err(e),
            }
        }
    }
}

// ---- private ----------------------------------------------------------------

async fn build_pool(config: &SqlxPgConfig) -> Result<PgPool, sqlx::Error> {
    let mut opts = PgPoolOptions::new();

    if let Some(v) = config.max_connections {
        opts = opts.max_connections(v);
    }
    if let Some(v) = config.min_connections {
        opts = opts.min_connections(v);
    }
    if let Some(v) = config.connect_timeout_secs {
        opts = opts.acquire_timeout(Duration::from_secs(v));
    }
    if let Some(v) = config.acquire_timeout_secs {
        opts = opts.acquire_timeout(Duration::from_secs(v));
    }
    if let Some(v) = config.idle_timeout_secs {
        opts = opts.idle_timeout(Duration::from_secs(v));
    }
    if let Some(v) = config.max_lifetime_secs {
        opts = opts.max_lifetime(Duration::from_secs(v));
    }

    opts = opts.test_before_acquire(config.test_before_acquire);

    if config.connect_lazy {
        opts.connect_lazy(&config.url())
    } else {
        opts.connect(&config.url()).await
    }
}
