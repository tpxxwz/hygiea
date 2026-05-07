use anyhow::{Context, Result};
use fred::prelude::*;
use fred::types::config::ClusterDiscoveryPolicy;
use hygiea_core::app::{Component, Resources, async_trait};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

// ============================================================
// Config
// ============================================================

#[derive(Clone, Debug, Deserialize)]
pub struct RedisNode {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct RedisConfig {
    /// Server mode: "standalone", "cluster", or "sentinel"
    pub mode: String,

    // ========== 连接信息 ==========
    pub host: String,
    pub port: u16,
    /// Cluster/Sentinel nodes
    pub nodes: Vec<RedisNode>,
    /// Sentinel service name (sentinel mode only)
    pub sentinel_service_name: String,
    /// Sentinel username (sentinel auth, optional)
    pub sentinel_username: String,
    /// Sentinel password (sentinel auth, optional)
    pub sentinel_password: String,

    // ========== 认证 ==========
    pub username: String,
    pub password: String,
    /// Redis database number (0-15, standalone/sentinel only)
    pub db: u8,

    // ========== 连接池 ==========
    pub pool_size: usize,

    // ========== 超时 ==========
    /// Connection timeout in seconds (0 = no timeout)
    pub connect_timeout_secs: u64,
    /// Default command timeout in seconds (0 = no timeout)
    pub command_timeout_secs: u64,

    // ========== 重试 ==========
    /// Max command attempts on failure (includes first attempt)
    pub max_command_attempts: u32,
    /// Max cluster MOVED/ASK redirections (cluster mode only)
    pub max_redirections: u32,

    // ========== 重连策略 ==========
    pub reconnect_max_attempts: u32,
    pub reconnect_min_delay_ms: u32,
    pub reconnect_max_delay_ms: u32,
    pub reconnect_multiplier: u32,

    // ========== 分布式锁 ==========
    /// Default TTL for distributed locks in seconds
    pub default_lock_timeout: u64,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            mode: "standalone".to_string(),
            host: "localhost".to_string(),
            port: 6379,
            nodes: Vec::new(),
            sentinel_service_name: "mymaster".to_string(),
            sentinel_username: String::new(),
            sentinel_password: String::new(),
            username: String::new(),
            password: String::new(),
            db: 0,
            pool_size: 5,
            connect_timeout_secs: 5,
            command_timeout_secs: 0,
            max_command_attempts: 3,
            max_redirections: 5,
            reconnect_max_attempts: 0,
            reconnect_min_delay_ms: 1,
            reconnect_max_delay_ms: 30_000,
            reconnect_multiplier: 2,
            default_lock_timeout: 30,
        }
    }
}

// ============================================================
// Pool wrapper
// ============================================================

#[derive(Clone, Debug)]
pub struct FredRedisPool {
    inner: Pool,
    #[cfg_attr(not(feature = "distributed-lock"), allow(dead_code))]
    config: Arc<RedisConfig>,
}

impl FredRedisPool {
    pub fn new(inner: Pool, config: Arc<RedisConfig>) -> Self {
        Self { inner, config }
    }

    pub async fn connect(config: RedisConfig) -> Result<Self> {
        let fred_config = build_fred_config(&config)?;
        let perf = build_perf_config(&config);
        let connection = build_connection_config(&config);
        let policy = ReconnectPolicy::new_exponential(
            config.reconnect_max_attempts,
            config.reconnect_min_delay_ms,
            config.reconnect_max_delay_ms,
            config.reconnect_multiplier,
        );
        let pool = Pool::new(
            fred_config,
            Some(perf),
            Some(connection),
            Some(policy),
            config.pool_size,
        )
        .context("Failed to create Redis pool")?;
        pool.init().await.context("Failed to connect to Redis")?;
        Ok(Self::new(pool, Arc::new(config)))
    }
}

impl std::ops::Deref for FredRedisPool {
    type Target = Pool;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ============================================================
// Component
// ============================================================

pub struct RedisComponent {
    config: RedisConfig,
}

#[async_trait]
impl Component for RedisComponent {
    type Config = RedisConfig;

    fn build(_name: &'static str, config: Self::Config) -> Self {
        Self { config }
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<Option<JoinHandle<()>>> {
        tracing::info!(
            "Connecting to Redis [mode={}] with pool_size={}",
            self.config.mode,
            self.config.pool_size
        );
        let pool = FredRedisPool::connect(self.config.clone()).await?;
        tracing::info!("Redis connection pool established");
        resources.insert(pool);
        Ok(None)
    }
}

// ============================================================
// Build helpers
// ============================================================

fn build_fred_config(config: &RedisConfig) -> Result<Config> {
    let server = match config.mode.as_str() {
        "standalone" => ServerConfig::Centralized {
            server: Server::new(&config.host, config.port),
        },
        "cluster" => {
            let hosts: Vec<Server> = config
                .nodes
                .iter()
                .map(|n| Server::new(&n.host, n.port))
                .collect();
            anyhow::ensure!(
                !hosts.is_empty(),
                "Cluster mode requires at least one node in 'nodes'"
            );
            ServerConfig::Clustered {
                hosts,
                policy: ClusterDiscoveryPolicy::default(),
            }
        }
        "sentinel" => {
            let hosts: Vec<Server> = config
                .nodes
                .iter()
                .map(|n| Server::new(&n.host, n.port))
                .collect();
            anyhow::ensure!(
                !hosts.is_empty(),
                "Sentinel mode requires at least one node in 'nodes'"
            );
            ServerConfig::Sentinel {
                hosts,
                service_name: config.sentinel_service_name.clone(),
                username: if config.sentinel_username.is_empty() {
                    None
                } else {
                    Some(config.sentinel_username.clone())
                },
                password: if config.sentinel_password.is_empty() {
                    None
                } else {
                    Some(config.sentinel_password.clone())
                },
            }
        }
        other => anyhow::bail!(
            "Invalid redis mode '{}'. Expected: standalone, cluster, sentinel",
            other
        ),
    };

    Ok(Config {
        server,
        username: if config.username.is_empty() {
            None
        } else {
            Some(config.username.clone())
        },
        password: if config.password.is_empty() {
            None
        } else {
            Some(config.password.clone())
        },
        database: if config.mode == "cluster" {
            None
        } else {
            Some(config.db)
        },
        ..Default::default()
    })
}

fn build_perf_config(config: &RedisConfig) -> PerformanceConfig {
    let mut perf = PerformanceConfig::default();
    if config.command_timeout_secs > 0 {
        perf.default_command_timeout = Duration::from_secs(config.command_timeout_secs);
    }
    perf
}

fn build_connection_config(config: &RedisConfig) -> ConnectionConfig {
    let mut conn = ConnectionConfig::default();
    if config.connect_timeout_secs > 0 {
        conn.connection_timeout = Duration::from_secs(config.connect_timeout_secs);
    }
    conn.max_command_attempts = config.max_command_attempts;
    conn.max_redirections = config.max_redirections;
    conn
}

// ============================================================
// DistributedLock
// ============================================================

#[cfg(feature = "distributed-lock")]
pub use hygiea_core::{DistributedKey, DistributedLock};

#[cfg(feature = "distributed-lock")]
mod distributed_lock {
    use super::*;
    use hygiea_core::{DistributedKey, DistributedLock};
    fn key_to_string(key: DistributedKey) -> String {
        match key {
            DistributedKey::Advisory(n) => format!("hygiea:distributed:lock:{n}"),
            DistributedKey::Advisory2(a, b) => format!("hygiea:distributed:lock:{a}:{b}"),
            DistributedKey::Named(s) => s,
        }
    }

    #[async_trait]
    impl DistributedLock for FredRedisPool {
        type Error = fred::error::Error;

        async fn lock<K, F, Fut, T>(&self, _key: K, _f: F) -> Result<T, Self::Error>
        where
            K: Into<DistributedKey> + Send,
            F: FnOnce() -> Fut + Send,
            Fut: std::future::Future<Output = Result<T, Self::Error>> + Send,
            T: Send + 'static,
        {
            unimplemented!("blocking lock is not yet implemented for Redis; use try_lock instead")
        }

        async fn try_lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<Option<T>, Self::Error>
        where
            K: Into<DistributedKey> + Send,
            F: FnOnce() -> Fut + Send,
            Fut: std::future::Future<Output = Result<T, Self::Error>> + Send,
            T: Send + 'static,
        {
            let redis_key = key_to_string(key.into());
            let ttl_ms = self.config.default_lock_timeout * 1000;

            let acquired: Option<String> = self
                .inner
                .set(
                    &redis_key,
                    1i64,
                    Some(Expiration::PX(ttl_ms as i64)),
                    Some(SetOptions::NX),
                    false,
                )
                .await?;

            if acquired.is_none() {
                return Ok(None);
            }

            let result = f().await;
            let _: () = self.inner.del(&redis_key).await?;
            result.map(Some)
        }
    }
}
