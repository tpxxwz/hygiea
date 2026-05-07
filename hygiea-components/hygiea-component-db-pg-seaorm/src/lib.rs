use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use serde::Deserialize;

use hygiea_core::app::{Component, Resources, async_trait};

// ---- config -----------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
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

    // ========== 分布式锁 ==========
    #[cfg(feature = "distributed-lock")]
    pub lock_table: String,
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
            #[cfg(feature = "distributed-lock")]
            lock_table: "hygiea_distributed_locks".to_string(),
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
    #[cfg_attr(not(feature = "distributed-lock"), allow(dead_code))]
    config: Arc<SeaOrmPgConfig>,
}

impl SeaOrmPgPool {
    pub fn new(inner: DatabaseConnection, config: Arc<SeaOrmPgConfig>) -> Self {
        Self { inner, config }
    }

    pub async fn connect(config: SeaOrmPgConfig) -> Result<Self, anyhow::Error> {
        let conn: DatabaseConnection = Database::connect(config.connect_options())
            .await
            .context("Failed to connect to database")?;
        let config = Arc::new(config);
        #[cfg(feature = "distributed-lock")]
        distributed_lock::ensure_lock_table(&conn, &config.lock_table).await?;
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

pub struct SeaOrmPgComponent {
    config: SeaOrmPgConfig,
}

#[async_trait]
impl Component for SeaOrmPgComponent {
    type Config = SeaOrmPgConfig;

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
        let pool = SeaOrmPgPool::connect(self.config.clone()).await?;
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

    pub(super) async fn ensure_lock_table(
        conn: &DatabaseConnection,
        table: &str,
    ) -> Result<(), anyhow::Error> {
        // key is VARCHAR(255): sufficient for any practical lock key (UUIDs, namespaced paths, etc.)
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (key VARCHAR(255) PRIMARY KEY)",
            table
        );
        sea_orm::sqlx::query(&sql)
            .execute(conn.get_postgres_connection_pool())
            .await
            .context("Failed to create lock table")?;
        tracing::info!("{} table ready", table);
        Ok(())
    }

    #[async_trait]
    impl DistributedLock for SeaOrmPgPool {
        type Error = sea_orm::sqlx::Error;

        async fn lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<T, Self::Error>
        where
            K: Into<DistributedKey> + Send,
            F: FnOnce() -> Fut + Send,
            Fut: Future<Output = Result<T, Self::Error>> + Send,
            T: Send + 'static,
        {
            let pool = self.inner.get_postgres_connection_pool();
            let mut tx = pool.begin().await?;
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
            let pool = self.inner.get_postgres_connection_pool();
            let mut tx = pool.begin().await?;
            if !try_acquire_lock(&mut tx, key.into(), &self.config.lock_table).await? {
                return Ok(None);
            }
            let result = f().await;
            tx.rollback().await?;
            result.map(Some)
        }
    }

    async fn acquire_lock(
        tx: &mut sea_orm::sqlx::Transaction<'_, sea_orm::sqlx::Postgres>,
        key: DistributedKey,
        lock_table: &str,
    ) -> Result<(), sea_orm::sqlx::Error> {
        match key {
            DistributedKey::Advisory(k) => {
                sea_orm::sqlx::query("SELECT pg_advisory_xact_lock($1)")
                    .bind(k)
                    .execute(&mut **tx)
                    .await?;
            }
            DistributedKey::Advisory2(a, b) => {
                sea_orm::sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
                    .bind(a)
                    .bind(b)
                    .execute(&mut **tx)
                    .await?;
            }
            DistributedKey::Named(k) => {
                sea_orm::sqlx::query(&format!(
                    "INSERT INTO {} (key) VALUES ($1) ON CONFLICT DO NOTHING",
                    lock_table
                ))
                .bind(&k)
                .execute(&mut **tx)
                .await?;
                sea_orm::sqlx::query(&format!(
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
        tx: &mut sea_orm::sqlx::Transaction<'_, sea_orm::sqlx::Postgres>,
        key: DistributedKey,
        lock_table: &str,
    ) -> Result<bool, sea_orm::sqlx::Error> {
        match key {
            DistributedKey::Advisory(k) => {
                let (ok,): (bool,) =
                    sea_orm::sqlx::query_as("SELECT pg_try_advisory_xact_lock($1)")
                        .bind(k)
                        .fetch_one(&mut **tx)
                        .await?;
                Ok(ok)
            }
            DistributedKey::Advisory2(a, b) => {
                let (ok,): (bool,) =
                    sea_orm::sqlx::query_as("SELECT pg_try_advisory_xact_lock($1, $2)")
                        .bind(a)
                        .bind(b)
                        .fetch_one(&mut **tx)
                        .await?;
                Ok(ok)
            }
            DistributedKey::Named(k) => {
                sea_orm::sqlx::query(&format!(
                    "INSERT INTO {} (key) VALUES ($1) ON CONFLICT DO NOTHING",
                    lock_table
                ))
                .bind(&k)
                .execute(&mut **tx)
                .await?;
                let result = sea_orm::sqlx::query(&format!(
                    "SELECT key FROM {} WHERE key = $1 FOR UPDATE NOWAIT",
                    lock_table
                ))
                .bind(&k)
                .execute(&mut **tx)
                .await;
                match result {
                    Ok(_) => Ok(true),
                    Err(sea_orm::sqlx::Error::Database(e))
                        if e.downcast_ref::<sea_orm::sqlx::postgres::PgDatabaseError>()
                            .code()
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

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_default() {
        let config = SeaOrmPgConfig::default();
        assert_eq!(
            config.url(),
            "postgres://postgres:postgres@localhost:5432/postgres"
        );
    }

    #[test]
    fn build_url_with_params() {
        let config = SeaOrmPgConfig {
            params: "sslmode=require".to_string(),
            ..SeaOrmPgConfig::default()
        };
        assert!(config.url().ends_with("?sslmode=require"));
    }

    #[test]
    fn build_url_no_params_no_question_mark() {
        let url = SeaOrmPgConfig::default().url();
        assert!(!url.contains('?'));
    }

    #[test]
    fn pgdb_deref_to_database_connection() {
        use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};

        let conn: DatabaseConnection =
            MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = SeaOrmPgPool::new(conn, Arc::new(SeaOrmPgConfig::default()));
        let _: &DatabaseConnection = &*db;
    }
}
