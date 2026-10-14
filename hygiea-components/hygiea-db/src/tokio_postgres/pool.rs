//! PostgreSQL 连接池组件（tokio-postgres），feature `tokio-postgres` + `postgres`。池是 deadpool-postgres；
//! 开了 `lock` 实现分布式锁、开了 `live-config` 有动态配置（LISTEN 推送），实现在 `lock/tokio_postgres/`、
//! `live_config/tokio_postgres/`

use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use serde::Deserialize;
use tokio_postgres::NoTls;

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use crate::config::{PG_POOL_SIZE, PgConnectConfig, PoolConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{
    LiveConfigOptions, LiveConfigSource, Store, impl_live_config, live_config_source_for,
};
#[cfg(feature = "lock")]
use crate::lock::{TokioPgLock, impl_pg_lock};

/// 配置文件里是 `[db.connect]`、`[db.pool]`（还有开了 feature 才有的 `[db.live_config]`），各段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TokioPgConfig {
    pub connect: PgConnectConfig,
    /// deadpool 只用到最大连接数和 acquire 超时，见 `PoolConfig::deadpool`
    pub pool: PoolConfig,
    #[cfg(feature = "live-config")]
    pub live_config: LiveConfigOptions,
}

/// PostgreSQL 连接池，`Deref` 到 deadpool-postgres 的 `Pool`。用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone)]
pub struct TokioPgPool {
    inner: Pool,
    /// 配置里没开是 `None`
    #[cfg(feature = "live-config")]
    live_config: Option<LiveConfigSource>,
    #[cfg(feature = "lock")]
    locker: TokioPgLock,
}

impl std::fmt::Debug for TokioPgPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioPgPool")
            .field("status", &self.inner.status())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ConfigResource for TokioPgPool {
    type Config = TokioPgConfig;

    async fn from_config(config: &TokioPgConfig, registry_config: &RegistryConfig) -> Result<Self> {
        let target = config.connect.target();
        let pg_config = config.connect.tokio_pg_config(registry_config)?;
        #[cfg(feature = "live-config")]
        let live_config = live_config_source_for(
            |tables| Ok(Store::tokio_postgres(pg_config.clone(), tables)),
            &config.live_config,
            registry_config,
        )?;
        let manager = Manager::from_config(
            pg_config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(manager)
            .config(config.pool.deadpool(PG_POOL_SIZE))
            .runtime(Runtime::Tokio1)
            .build()
            .wrap_err(|| err!(AppErr::InvalidConfig, "postgres pool"))?;
        // deadpool 本来就是第一次用时才连；不是 connect_lazy 时先借一条，连不上启动报错
        if !config.pool.connect_lazy {
            tracing::info!("Connecting to database: {target}");
            drop(
                pool.get()
                    .await
                    .wrap_err(|| err!(AppErr::ConnectFailed, target.clone()))?,
            );
            tracing::info!("Database connection established");
        }
        #[cfg(feature = "lock")]
        let locker = TokioPgLock::new(pool.clone(), registry_config)?;
        #[cfg(feature = "lock")]
        if !config.pool.connect_lazy {
            locker.prepare().await?;
        }
        Ok(Self {
            inner: pool,
            #[cfg(feature = "live-config")]
            live_config,
            #[cfg(feature = "lock")]
            locker,
        })
    }

    /// 不再发新连接，空闲的连接关掉，借出去的还回来时关掉；动态配置的后台任务也退出
    async fn close(&self) -> Result<()> {
        #[cfg(feature = "live-config")]
        if let Some(live_config) = &self.live_config {
            live_config.close().await;
        }
        self.inner.close();
        tracing::info!("Database connection pool closed");
        Ok(())
    }
}

#[cfg(feature = "live-config")]
impl_live_config!(TokioPgPool);

#[cfg(feature = "lock")]
impl_pg_lock!(TokioPgPool);

impl std::ops::Deref for TokioPgPool {
    type Target = Pool;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// PostgreSQL 连接池组件（tokio-postgres），按组件名放进 Resources 的是 [`TokioPgPool`]。用法见 [`ImmediateResourceComponent`]
pub type TokioPgComponent = ImmediateResourceComponent<TokioPgPool>;
