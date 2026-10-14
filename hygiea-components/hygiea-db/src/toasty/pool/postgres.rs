//! PostgreSQL 连接池组件（toasty），feature `toasty` + `postgres`。按 `[db.connect]` 连上，池参数用 toasty 的默认值。
//! 开了 `lock` 实现分布式锁、开了 `live-config` 有动态配置（只靠轮询），实现在 `lock/toasty/`、`live_config/toasty/`

use serde::Deserialize;

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use crate::config::PgConnectConfig;
#[cfg(feature = "live-config")]
use crate::live_config::{
    LiveConfigOptions, LiveConfigSource, Store, impl_live_config, live_config_source_for,
};
#[cfg(feature = "lock")]
use crate::lock::{ToastyPgLock, impl_pg_lock};

/// 配置文件里是 `[db.connect]`（还有开了 feature 才有的 `[db.live_config]`），不写用默认值。`schema_search_path` 不生效
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToastyPgConfig {
    pub connect: PgConnectConfig,
    #[cfg(feature = "live-config")]
    pub live_config: LiveConfigOptions,
}

/// PostgreSQL 连接池，`Deref` 到 toasty 的 `Db`。用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone)]
pub struct ToastyPgPool {
    inner: toasty::Db,
    /// 配置里没开是 `None`
    #[cfg(feature = "live-config")]
    live_config: Option<LiveConfigSource>,
    #[cfg(feature = "lock")]
    locker: ToastyPgLock,
}

impl std::fmt::Debug for ToastyPgPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToastyPgPool").finish_non_exhaustive()
    }
}

#[async_trait]
impl ConfigResource for ToastyPgPool {
    type Config = ToastyPgConfig;

    async fn from_config(
        config: &ToastyPgConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        let target = config.connect.target();
        let url = config.connect.url(registry_config)?;
        tracing::info!("Connecting to database: {target}");
        let inner = toasty::Db::builder()
            .connect(&url)
            .await
            .wrap_err(|| err!(AppErr::ConnectFailed, target))?;
        tracing::info!("Database connection established");
        #[cfg(feature = "live-config")]
        let live_config = live_config_source_for(
            |tables| Ok(Store::toasty_postgres(inner.clone(), tables)),
            &config.live_config,
            registry_config,
        )?;
        #[cfg(feature = "lock")]
        let locker = ToastyPgLock::new(inner.clone(), registry_config)?;
        #[cfg(feature = "lock")]
        locker.prepare().await?;
        Ok(Self {
            inner,
            #[cfg(feature = "live-config")]
            live_config,
            #[cfg(feature = "lock")]
            locker,
        })
    }

    /// toasty 没有关闭池的接口，最后一个 `Db` 被丢掉时连接才关；动态配置的后台任务在这里退出
    async fn close(&self) -> Result<()> {
        #[cfg(feature = "live-config")]
        if let Some(live_config) = &self.live_config {
            live_config.close().await;
        }
        Ok(())
    }
}

#[cfg(feature = "live-config")]
impl_live_config!(ToastyPgPool);

#[cfg(feature = "lock")]
impl_pg_lock!(ToastyPgPool);

impl std::ops::Deref for ToastyPgPool {
    type Target = toasty::Db;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// PostgreSQL 连接池组件（toasty），按组件名放进 Resources 的是 [`ToastyPgPool`]。用法见 [`ImmediateResourceComponent`]
pub type ToastyPgComponent = ImmediateResourceComponent<ToastyPgPool>;
