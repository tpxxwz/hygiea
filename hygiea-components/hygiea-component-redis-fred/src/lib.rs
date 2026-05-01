use anyhow::{Context, Result};
use fred::prelude::*;
use fred::types::config::ClusterDiscoveryPolicy;
use hygiea_core::app::{Component, Resources, async_trait};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

// ============================================================
// Config
// ============================================================

#[derive(Deserialize, Clone)]
pub struct RedisNode {
    pub host: String,
    pub port: u16,
}

#[derive(Deserialize, Clone)]
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
    /// Default TTL for locks in seconds; watchdog renews every timeout/3
    pub lock_watchdog_timeout: u64,
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
            lock_watchdog_timeout: 30,
        }
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
        let config = build_fred_config(&self.config)?;
        let perf = build_perf_config(&self.config);
        let connection = build_connection_config(&self.config);
        let policy = ReconnectPolicy::new_exponential(
            self.config.reconnect_max_attempts,
            self.config.reconnect_min_delay_ms,
            self.config.reconnect_max_delay_ms,
            self.config.reconnect_multiplier,
        );

        tracing::info!(
            "Connecting to Redis [mode={}] with pool_size={}",
            self.config.mode,
            self.config.pool_size
        );

        let pool = Pool::new(
            config,
            Some(perf),
            Some(connection),
            Some(policy),
            self.config.pool_size,
        )
        .context("Failed to create Redis pool")?;

        pool.init().await.context("Failed to connect to Redis")?;
        tracing::info!("Redis connection pool established");

        resources.insert(pool);

        Ok(None)
    }
}

// ============================================================
// Config Builders
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
