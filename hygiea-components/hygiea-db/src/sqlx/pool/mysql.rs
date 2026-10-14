//! MySQL 连接池组件（sqlx），feature `sqlx` + `mysql`。开了 `lock` 时实现分布式锁（`GET_LOCK`，只有 `try_lock` 是原生的）；
//! 开了 `live-config` 有动态配置（只靠轮询，MySQL 没有推送）

use serde::Deserialize;
use sqlx::MySqlPool;

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use crate::config::{MYSQL_POOL_SIZE, MySqlConnectConfig, PoolConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{
    LiveConfigOptions, LiveConfigSource, Store, impl_live_config, live_config_source_for,
};
#[cfg(feature = "lock")]
use crate::lock::{MySqlLock, impl_mysql_lock};

/// 配置文件里是 `[db.connect]`、`[db.pool]`（还有开了 feature 才有的 `[db.live_config]`），各段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlxMySqlConfig {
    pub connect: MySqlConnectConfig,
    pub pool: PoolConfig,
    #[cfg(feature = "live-config")]
    pub live_config: LiveConfigOptions,
}

/// MySQL 连接池，`Deref` 到 sqlx 的 `MySqlPool`。用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct SqlxMySqlPool {
    inner: MySqlPool,
    /// 配置里没开是 `None`
    #[cfg(feature = "live-config")]
    live_config: Option<LiveConfigSource>,
    #[cfg(feature = "lock")]
    locker: MySqlLock,
}

#[async_trait]
impl ConfigResource for SqlxMySqlPool {
    type Config = SqlxMySqlConfig;

    async fn from_config(
        config: &SqlxMySqlConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        let target = config.connect.target();
        let options = config.connect.sqlx_options(registry_config)?;
        #[cfg(feature = "live-config")]
        let live_config = live_config_source_for(
            |tables| Ok(Store::mysql(options.clone(), &config.pool, tables)),
            &config.live_config,
            registry_config,
        )?;
        tracing::info!("Connecting to database: {target}");
        let pool_options = config.pool.sqlx::<sqlx::MySql>(MYSQL_POOL_SIZE);
        let pool = if config.pool.connect_lazy {
            pool_options.connect_lazy_with(options)
        } else {
            pool_options
                .connect_with(options)
                .await
                .wrap_err(|| err!(AppErr::ConnectFailed, target.clone()))?
        };
        tracing::info!("Database connection established");
        Ok(Self {
            #[cfg(feature = "live-config")]
            live_config,
            #[cfg(feature = "lock")]
            locker: MySqlLock::new(pool.clone()),
            inner: pool,
        })
    }

    /// 不再发新连接，等借出去的连接还回来后全部关掉；动态配置的后台任务也退出
    async fn close(&self) -> Result<()> {
        #[cfg(feature = "live-config")]
        if let Some(live_config) = &self.live_config {
            live_config.close().await;
        }
        self.inner.close().await;
        tracing::info!("Database connection pool closed");
        Ok(())
    }
}

#[cfg(feature = "live-config")]
impl_live_config!(SqlxMySqlPool);

#[cfg(feature = "lock")]
impl_mysql_lock!(SqlxMySqlPool);

impl std::ops::Deref for SqlxMySqlPool {
    type Target = MySqlPool;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// MySQL 连接池组件（sqlx），按组件名放进 Resources 的是 [`SqlxMySqlPool`]。用法见 [`ImmediateResourceComponent`]
pub type SqlxMySqlComponent = ImmediateResourceComponent<SqlxMySqlPool>;
