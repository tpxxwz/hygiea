//! MySQL 连接池组件（sqlx），feature `sqlx` + `mysql`

use serde::Deserialize;
use sqlx::MySqlPool;

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use crate::config::{MYSQL_POOL_SIZE, MySqlConnectConfig, PoolConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{
    LiveConfigSource, MySqlLiveConfigConfig, Store, impl_live_config, live_config_source_for,
};
#[cfg(feature = "lock")]
use crate::lock::{MySqlLock, MySqlLockConfig, impl_lock};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db.connect]`、`[db.pool]`（还有开了 feature 才有的 `[db.live_config]`、`[db.lock]`），各段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlxMySqlConfig {
    pub connect: MySqlConnectConfig,
    pub pool: PoolConfig,
    #[cfg(feature = "live-config")]
    pub live_config: MySqlLiveConfigConfig,
    #[cfg(feature = "lock")]
    pub lock: MySqlLockConfig,
}

// ---- pool -------------------------------------------------------------------

/// MySQL 连接池，`Deref` 到 sqlx 的 `MySqlPool`。用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct SqlxMySqlPool {
    inner: MySqlPool,
    #[cfg(feature = "live-config")]
    /// 配置里没开是 `None`
    live_config: Option<LiveConfigSource>,
    /// 分布式锁，用主池或 `[db.lock]` 单独建的池
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
        #[cfg(feature = "lock")]
        let locker = MySqlLock::new(
            &pool,
            &config.connect,
            &config.pool,
            &config.lock,
            registry_config,
        )
        .await?;
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
impl_live_config!(SqlxMySqlPool);

#[cfg(feature = "lock")]
impl_lock!(SqlxMySqlPool);

impl std::ops::Deref for SqlxMySqlPool {
    type Target = MySqlPool;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// MySQL 连接池组件（sqlx），按组件名放进 Resources 的是 [`SqlxMySqlPool`]。用法见 [`ImmediateResourceComponent`]
pub type SqlxMySqlComponent = ImmediateResourceComponent<SqlxMySqlPool>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置模板 `config/sqlx-mysql.toml` 就是代码里的默认值（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(all(feature = "live-config", feature = "lock"))]
    #[test]
    fn template_is_code_defaults() {
        let config: SqlxMySqlConfig =
            crate::config::parse_template(include_str!("../../../config/sqlx-mysql.toml"));
        assert_eq!(
            format!("{config:?}"),
            format!("{:?}", SqlxMySqlConfig::default())
        );
    }

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        use hygiea_core::app::{Component, Name, ResourceId};

        let named = SqlxMySqlComponent::build(Name::from("primary"), SqlxMySqlConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<SqlxMySqlPool>("primary")]
        );
        let anonymous = SqlxMySqlComponent::build(Name::from(""), SqlxMySqlConfig::default());
        assert_eq!(
            anonymous.provides(),
            vec![ResourceId::of::<SqlxMySqlPool>()]
        );
    }

    #[tokio::test]
    async fn connect_lazy_to_nonexistent_host_succeeds() {
        // 连不到的主机，connect_lazy 也能建出池子，第一次用时才连
        let mut config = SqlxMySqlConfig::default();
        config.connect.host = "nonexistent-host-xyz-12345.invalid".to_string();
        config.pool.connect_lazy = true;
        assert!(
            SqlxMySqlPool::from_config(&config, &Default::default())
                .await
                .is_ok()
        );
    }
}
