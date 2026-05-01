use std::future::Future;
use std::time::Duration;

use anyhow::Context;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use serde::Deserialize;

use hygiea_core::app::{Component, Resources, async_trait};

// ---- config -----------------------------------------------------------------

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct SeaOrmPgConfig {
    /// Protocol/Scheme (e.g., "postgres", "postgresql")
    pub scheme: String,
    /// Database host
    pub host: String,
    /// Database port
    pub port: u16,
    /// Database username
    pub username: String,
    /// Database password
    pub password: String,
    /// Database name
    pub database: String,
    /// Optional URL parameters (e.g., "sslmode=require")
    pub params: String,

    // ========== SeaORM Options ==========
    /// Schema search path (PostgreSQL only, empty = use database default)
    pub schema_search_path: String,
    /// Schema for distributed_locks table (empty = first of schema_search_path, or "public")
    pub lock_schema: String,

    // ========== 连接池 ==========
    /// Maximum number of connections in the pool (None = use SQLx default)
    pub max_connections: Option<u32>,
    /// Minimum number of connections in the pool (None = use SQLx default)
    pub min_connections: Option<u32>,

    // ========== 超时 ==========
    /// Connection timeout in seconds (None = use SQLx default)
    pub connect_timeout_secs: Option<u64>,
    /// Acquire timeout in seconds (None = use SQLx default)
    pub acquire_timeout_secs: Option<u64>,
    /// Idle timeout in seconds (None = use SQLx default)
    pub idle_timeout_secs: Option<u64>,
    /// Max lifetime of a connection in seconds (None = use SQLx default)
    pub max_lifetime_secs: Option<u64>,

    // ========== 连接池行为 ==========
    /// Test connection before acquiring from pool (default: true)
    pub test_before_acquire: bool,
    /// Lazily establish connections (default: false)
    pub connect_lazy: bool,

    // ========== 日志 ==========
    /// Enable SQLx statement logging (default: true)
    pub sqlx_logging: bool,
    /// SQLx logging level (default: "info")
    pub sqlx_logging_level: String,
    /// SQLx slow statements logging level (default: "off")
    pub sqlx_slow_statements_logging_level: String,
    /// SQLx slow statements threshold in seconds (default: 1)
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
            lock_schema: String::new(),
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

        let opt = build_connect_options(&self.config);
        let conn: DatabaseConnection = Database::connect(opt)
            .await
            .context("Failed to connect to database")?;

        tracing::info!("Database connection established");

        resources.insert(PgDb::new(
            conn,
            self.config.effective_lock_schema().to_string(),
        ));

        Ok(None)
    }
}

// ---- private ----------------------------------------------------------------

pub fn build_url(config: &SeaOrmPgConfig) -> String {
    let params_part = if config.params.is_empty() { "" } else { "?" };
    format!(
        "{}://{}:{}@{}:{}/{}{}{}",
        config.scheme,
        config.username,
        config.password,
        config.host,
        config.port,
        config.database,
        params_part,
        config.params
    )
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

fn build_connect_options(config: &SeaOrmPgConfig) -> ConnectOptions {
    let mut opt = ConnectOptions::new(build_url(config));

    if !config.schema_search_path.is_empty() {
        opt.set_schema_search_path(&config.schema_search_path);
    }

    if let Some(v) = config.max_connections {
        opt.max_connections(v);
    }
    if let Some(v) = config.min_connections {
        opt.min_connections(v);
    }

    if let Some(v) = config.connect_timeout_secs {
        opt.connect_timeout(Duration::from_secs(v));
    }
    if let Some(v) = config.acquire_timeout_secs {
        opt.acquire_timeout(Duration::from_secs(v));
    }
    if let Some(v) = config.idle_timeout_secs {
        opt.idle_timeout(Duration::from_secs(v));
    }
    if let Some(v) = config.max_lifetime_secs {
        opt.max_lifetime(Duration::from_secs(v));
    }

    opt.test_before_acquire(config.test_before_acquire);
    opt.connect_lazy(config.connect_lazy);
    opt.sqlx_logging(config.sqlx_logging);
    opt.sqlx_logging_level(parse_log_level(&config.sqlx_logging_level));
    opt.sqlx_slow_statements_logging_settings(
        parse_log_level(&config.sqlx_slow_statements_logging_level),
        Duration::from_secs(config.sqlx_slow_statements_threshold_secs),
    );

    opt
}

// ---- PgDb -------------------------------------------------------------------

#[cfg(feature = "distributed-lock")]
pub use hygiea_core::{DistributedKey, DistributedLock};

pub struct PgDb {
    pub inner: DatabaseConnection,
    pub lock_schema: String,
}

impl PgDb {
    pub fn new(inner: DatabaseConnection, lock_schema: String) -> Self {
        Self { inner, lock_schema }
    }
}

impl std::ops::Deref for PgDb {
    type Target = DatabaseConnection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(feature = "distributed-lock")]
#[async_trait]
impl DistributedLock for PgDb {
    type Error = sea_orm::sqlx::Error;

    async fn with_lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<T, Self::Error>
    where
        K: Into<DistributedKey> + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<T, Self::Error>> + Send,
        T: Send + 'static,
    {
        let pool = self.inner.get_postgres_connection_pool();
        let mut tx = pool.begin().await?;
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
        let pool = self.inner.get_postgres_connection_pool();
        let mut tx = pool.begin().await?;
        if !try_acquire_lock(&mut tx, key.into(), &self.lock_schema).await? {
            return Ok(None);
        }
        let result = f().await;
        tx.rollback().await?;
        result.map(Some)
    }
}

#[cfg(feature = "distributed-lock")]
async fn acquire_lock(
    tx: &mut sea_orm::sqlx::Transaction<'_, sea_orm::sqlx::Postgres>,
    key: DistributedKey,
    lock_schema: &str,
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
            let table = format!("{}.distributed_locks", lock_schema);
            sea_orm::sqlx::query(&format!(
                "INSERT INTO {} (key) VALUES ($1) ON CONFLICT DO NOTHING",
                table
            ))
            .bind(&k)
            .execute(&mut **tx)
            .await?;
            sea_orm::sqlx::query(&format!(
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

#[cfg(feature = "distributed-lock")]
async fn try_acquire_lock(
    tx: &mut sea_orm::sqlx::Transaction<'_, sea_orm::sqlx::Postgres>,
    key: DistributedKey,
    lock_schema: &str,
) -> Result<bool, sea_orm::sqlx::Error> {
    match key {
        DistributedKey::Advisory(k) => {
            let (ok,): (bool,) = sea_orm::sqlx::query_as("SELECT pg_try_advisory_xact_lock($1)")
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
            let table = format!("{}.distributed_locks", lock_schema);
            sea_orm::sqlx::query(&format!(
                "INSERT INTO {} (key) VALUES ($1) ON CONFLICT DO NOTHING",
                table
            ))
            .bind(&k)
            .execute(&mut **tx)
            .await?;
            let result = sea_orm::sqlx::query(&format!(
                "SELECT key FROM {} WHERE key = $1 FOR UPDATE NOWAIT",
                table
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

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_default() {
        let config = SeaOrmPgConfig::default();
        assert_eq!(
            build_url(&config),
            "postgres://postgres:postgres@localhost:5432/postgres"
        );
    }

    #[test]
    fn build_url_with_params() {
        let config = SeaOrmPgConfig {
            params: "sslmode=require".to_string(),
            ..SeaOrmPgConfig::default()
        };
        assert!(build_url(&config).ends_with("?sslmode=require"));
    }

    #[test]
    fn build_url_no_params_no_question_mark() {
        let url = build_url(&SeaOrmPgConfig::default());
        assert!(!url.contains('?'));
    }

    #[test]
    fn pgdb_deref_to_database_connection() {
        use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};

        let conn: DatabaseConnection =
            MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let db = PgDb::new(conn, "public".to_string());
        let _: &DatabaseConnection = &*db;
    }
}
