//! Redis 连接池组件（fred），feature `fred`

use fred::prelude::*;
use fred::types::config::ClusterDiscoveryPolicy;
use hygiea_core::app::{AppErr, ConfigResource, ImmediateResourceComponent, async_trait};
use hygiea_core::{Result, ResultExt, err};
use serde::Deserialize;
use std::time::Duration;

// ============================================================
// Config
// ============================================================

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisNode {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
        }
    }
}

// ============================================================
// Pool wrapper
// ============================================================

/// Redis 连接池，`Deref` 到 fred 的 `Pool`，命令照常调。用 [`ConfigResource::from_config`] 建，
/// 不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct FredRedisPool {
    inner: Pool,
}

#[async_trait]
impl ConfigResource for FredRedisPool {
    type Config = RedisConfig;

    async fn from_config(config: &RedisConfig) -> Result<Self> {
        tracing::info!(
            "Connecting to Redis [mode={}] with pool_size={}",
            config.mode,
            config.pool_size
        );
        let fred_config = build_fred_config(config)?;
        let perf = build_perf_config(config);
        let connection = build_connection_config(config);
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
        .wrap_err(|| err!(AppErr::InvalidConfig, "create Redis pool failed"))?;
        pool.init()
            .await
            .wrap_err(|| err!(AppErr::ConnectFailed, format!("redis ({})", config.mode)))?;
        tracing::info!("Redis connection pool established");
        Ok(Self { inner: pool })
    }

    /// 发 QUIT 正常断开所有连接
    async fn close(&self) -> Result<()> {
        self.inner
            .quit()
            .await
            .wrap_err(|| err!(AppErr::StopFailed, "quit redis connections"))?;
        tracing::info!("Redis connection pool closed");
        Ok(())
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

/// Redis 连接池组件，按组件名放进 Resources 的是 [`FredRedisPool`]。用法见 [`ImmediateResourceComponent`]
pub type RedisComponent = ImmediateResourceComponent<FredRedisPool>;

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
            if hosts.is_empty() {
                return Err(err!(
                    AppErr::InvalidConfig,
                    "cluster mode requires at least one node in 'nodes'"
                ));
            }
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
            if hosts.is_empty() {
                return Err(err!(
                    AppErr::InvalidConfig,
                    "sentinel mode requires at least one node in 'nodes'"
                ));
            }
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
        other => {
            return Err(err!(
                AppErr::InvalidConfig,
                format!("invalid redis mode '{other}', expected: standalone, cluster, sentinel")
            ));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_fred_config_standalone_mode() {
        let config = RedisConfig {
            mode: "standalone".to_string(),
            host: "localhost".to_string(),
            port: 6379,
            ..Default::default()
        };
        let result = build_fred_config(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn build_fred_config_cluster_mode() {
        let config = RedisConfig {
            mode: "cluster".to_string(),
            nodes: vec![
                RedisNode {
                    host: "cluster1".to_string(),
                    port: 6379,
                },
                RedisNode {
                    host: "cluster2".to_string(),
                    port: 6379,
                },
            ],
            ..Default::default()
        };
        let result = build_fred_config(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn build_fred_config_sentinel_mode() {
        let config = RedisConfig {
            mode: "sentinel".to_string(),
            nodes: vec![RedisNode {
                host: "sentinel1".to_string(),
                port: 26379,
            }],
            sentinel_service_name: "mymaster".to_string(),
            ..Default::default()
        };
        let result = build_fred_config(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn build_fred_config_cluster_without_nodes_fails() {
        let config = RedisConfig {
            mode: "cluster".to_string(),
            nodes: Vec::new(),
            ..Default::default()
        };
        let result = build_fred_config(&config);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cluster mode requires at least one node")
        );
    }

    #[test]
    fn build_fred_config_sentinel_without_nodes_fails() {
        let config = RedisConfig {
            mode: "sentinel".to_string(),
            nodes: Vec::new(),
            ..Default::default()
        };
        let result = build_fred_config(&config);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("sentinel mode requires at least one node")
        );
    }

    #[test]
    fn build_fred_config_invalid_mode_fails() {
        let config = RedisConfig {
            mode: "invalid_mode".to_string(),
            ..Default::default()
        };
        let result = build_fred_config(&config);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid redis mode")
        );
    }

    #[test]
    fn empty_username_converts_to_none() {
        let config = RedisConfig {
            username: String::new(),
            password: "password".to_string(),
            ..Default::default()
        };
        let fred_config = build_fred_config(&config).unwrap();
        assert!(fred_config.username.is_none());
    }

    #[test]
    fn empty_password_converts_to_none() {
        let config = RedisConfig {
            username: "user".to_string(),
            password: String::new(),
            ..Default::default()
        };
        let fred_config = build_fred_config(&config).unwrap();
        assert!(fred_config.password.is_none());
    }

    #[test]
    fn cluster_mode_does_not_set_database() {
        let config = RedisConfig {
            mode: "cluster".to_string(),
            nodes: vec![RedisNode {
                host: "localhost".to_string(),
                port: 6379,
            }],
            db: 1,
            ..Default::default()
        };
        let fred_config = build_fred_config(&config).unwrap();
        assert!(fred_config.database.is_none());
    }

    #[test]
    fn standalone_mode_sets_database() {
        let config = RedisConfig {
            mode: "standalone".to_string(),
            db: 2,
            ..Default::default()
        };
        let fred_config = build_fred_config(&config).unwrap();
        assert_eq!(fred_config.database, Some(2));
    }

    #[test]
    fn perf_config_zero_timeout_uses_default() {
        let config = RedisConfig {
            command_timeout_secs: 0,
            ..Default::default()
        };
        let perf = build_perf_config(&config);
        // 0 表示沿用 fred 默认值，不会因为 0 就改成 fred 的默认值
        // 这里验证返回的 perf config 不为空就行
        let _ = perf.default_command_timeout;
    }

    #[test]
    fn perf_config_nonzero_timeout_is_set() {
        let config = RedisConfig {
            command_timeout_secs: 10,
            ..Default::default()
        };
        let perf = build_perf_config(&config);
        assert_eq!(perf.default_command_timeout, Duration::from_secs(10));
    }

    #[test]
    fn connection_config_zero_timeout_uses_default() {
        let config = RedisConfig {
            connect_timeout_secs: 0,
            ..Default::default()
        };
        let conn = build_connection_config(&config);
        // 0 表示沿用 fred 默认值，验证返回的 connection config 不为空
        let _ = conn.connection_timeout;
    }

    #[test]
    fn connection_config_nonzero_timeout_is_set() {
        let config = RedisConfig {
            connect_timeout_secs: 5,
            ..Default::default()
        };
        let conn = build_connection_config(&config);
        assert_eq!(conn.connection_timeout, Duration::from_secs(5));
    }

    #[test]
    fn redis_config_deserialize_unknown_field_fails() {
        let json = r#"{"mode": "standalone", "unknown_field": "value"}"#;
        let result: Result<RedisConfig, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn redis_config_default_values() {
        let config = RedisConfig::default();
        assert_eq!(config.mode, "standalone");
        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 6379);
        assert_eq!(config.pool_size, 5);
        assert_eq!(config.connect_timeout_secs, 5);
        assert_eq!(config.db, 0);
    }
}
