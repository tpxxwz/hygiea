//! MySQL 连接组件（SeaORM），feature `seaorm` + `mysql`

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use serde::Deserialize;

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use super::{SeaOrmLogConfig, connect_options};
use crate::config::{MYSQL_POOL_SIZE, MySqlConnectConfig, PoolConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{
    LiveConfigSource, MySqlLiveConfigConfig, Store, impl_live_config, live_config_source_for,
};
#[cfg(feature = "lock")]
use crate::lock::{MySqlLock, MySqlLockConfig, impl_lock};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db.connect]`、`[db.pool]`、`[db.log]`（还有开了 feature 才有的 `[db.live_config]`、`[db.lock]`），各段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeaOrmMySqlConfig {
    pub connect: MySqlConnectConfig,
    pub pool: PoolConfig,
    pub log: SeaOrmLogConfig,
    #[cfg(feature = "live-config")]
    pub live_config: MySqlLiveConfigConfig,
    #[cfg(feature = "lock")]
    pub lock: MySqlLockConfig,
}

impl SeaOrmMySqlConfig {
    fn connect_options(&self, registry_config: &RegistryConfig) -> Result<ConnectOptions> {
        Ok(connect_options(
            self.connect.url(registry_config)?,
            &self.pool,
            &self.log,
            MYSQL_POOL_SIZE,
        ))
    }
}

// ---- pool -------------------------------------------------------------------

/// MySQL 连接（SeaORM），`Deref` 到 `DatabaseConnection`。用 [`ConfigResource::from_config`] 建，
/// 不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct SeaOrmMySqlPool {
    inner: DatabaseConnection,
    #[cfg(feature = "live-config")]
    /// 配置里没开是 `None`
    live_config: Option<LiveConfigSource>,
    /// 分布式锁，用底层的 sqlx 池或 `[db.lock]` 单独建的池
    #[cfg(feature = "lock")]
    locker: MySqlLock,
}

#[async_trait]
impl ConfigResource for SeaOrmMySqlPool {
    type Config = SeaOrmMySqlConfig;

    async fn from_config(
        config: &SeaOrmMySqlConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        #[cfg(feature = "live-config")]
        let live_config = live_config_source_for(
            |tables| {
                let connect = config.live_config.connect.merge(&config.connect);
                Ok(Store::mysql(
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
        // Database::connect 按 mysql:// 建的，一定是 MySQL 连接池，get_mysql_connection_pool 不会 panic
        #[cfg(feature = "lock")]
        let locker = MySqlLock::new(
            conn.get_mysql_connection_pool(),
            &config.connect,
            &config.pool,
            &config.lock,
            registry_config,
        )
        .await?;
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
impl_live_config!(SeaOrmMySqlPool);

#[cfg(feature = "lock")]
impl_lock!(SeaOrmMySqlPool);

impl std::ops::Deref for SeaOrmMySqlPool {
    type Target = DatabaseConnection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// MySQL 连接组件（SeaORM），按组件名放进 Resources 的是 [`SeaOrmMySqlPool`]。用法见 [`ImmediateResourceComponent`]
pub type SeaOrmMySqlComponent = ImmediateResourceComponent<SeaOrmMySqlPool>;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 配置模板 `config/seaorm-mysql.toml` 就是代码里的默认值（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(all(feature = "live-config", feature = "lock"))]
    #[test]
    fn template_is_code_defaults() {
        let config: SeaOrmMySqlConfig =
            crate::config::parse_template(include_str!("../../../config/seaorm-mysql.toml"));
        assert_eq!(
            format!("{config:?}"),
            format!("{:?}", SeaOrmMySqlConfig::default())
        );
    }

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        use hygiea_core::app::{Component, Name, ResourceId};

        let named =
            SeaOrmMySqlComponent::build(Name::from("primary"), SeaOrmMySqlConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<SeaOrmMySqlPool>("primary")]
        );
        let anonymous = SeaOrmMySqlComponent::build(Name::from(""), SeaOrmMySqlConfig::default());
        assert_eq!(
            anonymous.provides(),
            vec![ResourceId::of::<SeaOrmMySqlPool>()]
        );
    }

    /// 池参数不写时用 MySQL 的默认值，写了的按写的
    #[test]
    fn pool_options_applied() {
        let config = SeaOrmMySqlConfig {
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
