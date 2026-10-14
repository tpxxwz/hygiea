//! PostgreSQL 连接组件（SeaORM），feature `seaorm` + `postgres`

use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use serde::Deserialize;

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use crate::config::{PG_POOL_SIZE, PgConnectConfig, PoolConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{
    LiveConfigSource, PgLiveConfigConfig, Store, impl_live_config, live_config_source_for,
};
#[cfg(feature = "lock")]
use crate::lock::{LockConfig, PgLock, impl_pg_lock};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db.connect]`、`[db.pool]`、`[db.log]`（还有开了 feature 才有的 `[db.live_config]`、`[db.lock]`），各段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeaOrmPgConfig {
    pub connect: PgConnectConfig,
    pub pool: PoolConfig,
    pub log: SeaOrmLogConfig,
    #[cfg(feature = "live-config")]
    pub live_config: PgLiveConfigConfig,
    #[cfg(feature = "lock")]
    pub lock: LockConfig,
}

/// SeaORM 打的 SQL 日志。级别是 trace / debug / info / warn / error / off
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeaOrmLogConfig {
    /// 每条 SQL 都打
    pub enabled: bool,
    pub level: String,
    /// 慢 SQL 单独打的级别，`off` 不打
    pub slow_statements_level: String,
    pub slow_statements_threshold_secs: u64,
}

impl Default for SeaOrmLogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: "info".to_string(),
            slow_statements_level: "off".to_string(),
            slow_statements_threshold_secs: 1,
        }
    }
}

impl SeaOrmPgConfig {
    fn connect_options(&self, registry_config: &RegistryConfig) -> Result<ConnectOptions> {
        let (connect, pool, log) = (&self.connect, &self.pool, &self.log);
        let mut opt = ConnectOptions::new(connect.url(registry_config)?);
        if !connect.schema_search_path.is_empty() {
            opt.set_schema_search_path(&connect.schema_search_path);
        }
        opt.max_connections(pool.max_connections(PG_POOL_SIZE))
            .min_connections(pool.min_connections(PG_POOL_SIZE))
            .acquire_timeout(Duration::from_secs(pool.acquire_timeout_secs()))
            .idle_timeout(Duration::from_secs(pool.idle_timeout_secs()))
            .max_lifetime(Duration::from_secs(pool.max_lifetime_secs()))
            .test_before_acquire(pool.test_before_acquire)
            .connect_lazy(pool.connect_lazy)
            .sqlx_logging(log.enabled)
            .sqlx_logging_level(parse_log_level(&log.level))
            .sqlx_slow_statements_logging_settings(
                parse_log_level(&log.slow_statements_level),
                Duration::from_secs(log.slow_statements_threshold_secs),
            );
        Ok(opt)
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

/// PostgreSQL 连接（SeaORM），`Deref` 到 `DatabaseConnection`。用 [`ConfigResource::from_config`] 建，
/// 不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct SeaOrmPgPool {
    inner: DatabaseConnection,
    #[cfg(feature = "live-config")]
    /// 配置里没开是 `None`
    live_config: Option<LiveConfigSource>,
    /// 分布式锁，用底层的 sqlx 池
    #[cfg(feature = "lock")]
    locker: PgLock,
}

#[async_trait]
impl ConfigResource for SeaOrmPgPool {
    type Config = SeaOrmPgConfig;

    async fn from_config(
        config: &SeaOrmPgConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        #[cfg(feature = "live-config")]
        let live_config = live_config_source_for(
            |tables| {
                let connect = config.live_config.connect.merge(&config.connect);
                Ok(Store::postgres(
                    connect.sqlx_options(registry_config)?,
                    &config.pool,
                    tables,
                ))
            },
            &config.live_config,
            registry_config,
        )?;
        let target = config.connect.target();
        tracing::info!("Connecting to database: {target}");
        let conn = Database::connect(config.connect_options(registry_config)?)
            .await
            .wrap_err(|| err!(AppErr::ConnectFailed, target.clone()))?;
        tracing::info!("Database connection established");
        // Database::connect 按 postgres:// 建的，一定是 PG 连接池，get_postgres_connection_pool 不会 panic
        #[cfg(feature = "lock")]
        let locker = PgLock::new(
            conn.get_postgres_connection_pool(),
            &config.connect,
            &config.pool,
            &config.lock,
            registry_config,
        )
        .await?;
        #[cfg(feature = "lock")]
        if !config.pool.connect_lazy {
            locker.prepare().await?;
        }
        Ok(Self {
            inner: conn,
            #[cfg(feature = "live-config")]
            live_config,
            #[cfg(feature = "lock")]
            locker,
        })
    }

    /// 等在途查询跑完后关掉连接池；动态配置的后台任务也退出，锁单独的池也关掉
    async fn close(&self) -> Result<()> {
        #[cfg(feature = "live-config")]
        if let Some(live_config) = &self.live_config {
            live_config.close().await;
        }
        #[cfg(feature = "lock")]
        self.locker.close().await;
        self.inner
            .close_by_ref()
            .await
            .wrap_err(|| err!(AppErr::StopFailed, "close database connection"))?;
        tracing::info!("Database connection pool closed");
        Ok(())
    }
}

#[cfg(feature = "live-config")]
impl_live_config!(SeaOrmPgPool);

#[cfg(feature = "lock")]
impl_pg_lock!(SeaOrmPgPool);

impl std::ops::Deref for SeaOrmPgPool {
    type Target = DatabaseConnection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// PostgreSQL 连接组件（SeaORM），按组件名放进 Resources 的是 [`SeaOrmPgPool`]。用法见 [`ImmediateResourceComponent`]
pub type SeaOrmPgComponent = ImmediateResourceComponent<SeaOrmPgPool>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置模板 `config/seaorm-pg.toml` 就是代码里的默认值（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(all(feature = "live-config", feature = "lock"))]
    #[test]
    fn template_is_code_defaults() {
        let config: SeaOrmPgConfig =
            crate::config::parse_template(include_str!("../../../config/seaorm-pg.toml"));
        assert_eq!(
            format!("{config:?}"),
            format!("{:?}", SeaOrmPgConfig::default())
        );
    }

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        use hygiea_core::app::{Component, Name, ResourceId};

        let named = SeaOrmPgComponent::build(Name::from("primary"), SeaOrmPgConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<SeaOrmPgPool>("primary")]
        );
        let anonymous = SeaOrmPgComponent::build(Name::from(""), SeaOrmPgConfig::default());
        assert_eq!(anonymous.provides(), vec![ResourceId::of::<SeaOrmPgPool>()]);
    }

    /// 池参数不写时用 PG 的默认值，写了的按写的
    #[test]
    fn pool_options_applied() {
        let config = SeaOrmPgConfig {
            pool: PoolConfig {
                max_connections: Some(3),
                ..Default::default()
            },
            ..Default::default()
        };
        let options = config.connect_options(&RegistryConfig::default()).unwrap();
        assert_eq!(options.get_max_connections(), Some(3));
        assert_eq!(options.get_min_connections(), Some(0));
        assert_eq!(options.get_acquire_timeout(), Some(Duration::from_secs(30)));
    }
}
