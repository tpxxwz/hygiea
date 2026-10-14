//! PostgreSQL 连接池组件（sqlx），feature `sqlx` + `postgres`

use serde::Deserialize;
use sqlx::PgPool;

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
use crate::lock::{PgLock, PgLockConfig, impl_lock};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db.connect]`、`[db.pool]`（还有开了 feature 才有的 `[db.live_config]`、`[db.lock]`），各段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlxPgConfig {
    pub connect: PgConnectConfig,
    pub pool: PoolConfig,
    #[cfg(feature = "live-config")]
    pub live_config: PgLiveConfigConfig,
    #[cfg(feature = "lock")]
    pub lock: PgLockConfig,
}

// ---- pool -------------------------------------------------------------------

/// PostgreSQL 连接池，`Deref` 到 sqlx 的 `PgPool`。用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct SqlxPgPool {
    inner: PgPool,
    #[cfg(feature = "live-config")]
    /// 配置里没开是 `None`
    live_config: Option<LiveConfigSource>,
    /// 分布式锁，用主池或 `[db.lock]` 单独建的池
    #[cfg(feature = "lock")]
    locker: PgLock,
}

#[async_trait]
impl ConfigResource for SqlxPgPool {
    type Config = SqlxPgConfig;

    async fn from_config(config: &SqlxPgConfig, registry_config: &RegistryConfig) -> Result<Self> {
        let target = config.connect.target();
        let options = config.connect.sqlx_options(registry_config)?;
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
        tracing::info!("Connecting to database: {target}");
        let pool_options = config.pool.sqlx::<sqlx::Postgres>(PG_POOL_SIZE);
        let pool = if config.pool.connect_lazy {
            pool_options.connect_lazy_with(options)
        } else {
            pool_options
                .connect_with(options)
                .await
                .wrap_err(|| err!(AppErr::ConnectFailed, target.clone()))?
        };
        tracing::info!("Database connection established");
        #[cfg(feature = "lock")]
        let locker = PgLock::new(
            &pool,
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
            inner: pool,
            #[cfg(feature = "live-config")]
            live_config,
            #[cfg(feature = "lock")]
            locker,
        })
    }

    /// 不再发新连接，等借出去的连接还回来（在途查询跑完）后全部关掉；动态配置的后台任务也退出，锁单独的池也关掉
    async fn close(&self) -> Result<()> {
        #[cfg(feature = "live-config")]
        if let Some(live_config) = &self.live_config {
            live_config.close().await;
        }
        #[cfg(feature = "lock")]
        self.locker.close().await;
        self.inner.close().await;
        tracing::info!("Database connection pool closed");
        Ok(())
    }
}

#[cfg(feature = "live-config")]
impl_live_config!(SqlxPgPool);

#[cfg(feature = "lock")]
impl_lock!(SqlxPgPool);

impl std::ops::Deref for SqlxPgPool {
    type Target = PgPool;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// PostgreSQL 连接池组件（sqlx），按组件名放进 Resources 的是 [`SqlxPgPool`]。用法见 [`ImmediateResourceComponent`]
pub type SqlxPgComponent = ImmediateResourceComponent<SqlxPgPool>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置模板 `config/sqlx-pg.toml` 就是代码里的默认值（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(all(feature = "live-config", feature = "lock"))]
    #[test]
    fn template_is_code_defaults() {
        let config: SqlxPgConfig =
            crate::config::parse_template(include_str!("../../../config/sqlx-pg.toml"));
        assert_eq!(
            format!("{config:?}"),
            format!("{:?}", SqlxPgConfig::default())
        );
    }

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        use hygiea_core::app::{Component, Name, ResourceId};

        let named = SqlxPgComponent::build(Name::from("primary"), SqlxPgConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<SqlxPgPool>("primary")]
        );
        let anonymous = SqlxPgComponent::build(Name::from(""), SqlxPgConfig::default());
        assert_eq!(anonymous.provides(), vec![ResourceId::of::<SqlxPgPool>()]);
    }

    #[tokio::test]
    async fn connect_lazy_to_nonexistent_host_succeeds() {
        // 连不到的主机，connect_lazy 也能建出池子，第一次用时才连
        let mut config = SqlxPgConfig::default();
        config.connect.host = "nonexistent-host-xyz-12345.invalid".to_string();
        config.pool.connect_lazy = true;
        assert!(
            SqlxPgPool::from_config(&config, &Default::default())
                .await
                .is_ok()
        );
    }

    /// search_path 带进 sqlx 的连接参数（不在 URL 里）
    #[test]
    fn search_path_goes_into_options() {
        let connect = PgConnectConfig {
            schema_search_path: "public,custom".to_string(),
            ..Default::default()
        };
        let options = connect.sqlx_options(&RegistryConfig::default()).unwrap();
        assert_eq!(options.get_options(), Some("-c search_path=public,custom"));
    }
}
