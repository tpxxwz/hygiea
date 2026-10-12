//! PostgreSQL 连接池组件（sqlx），feature `sqlx` + `postgres`

use serde::Deserialize;
use sqlx::PgPool;

use hygiea_core::app::{
    AppContext, AppErr, ConfigResource, ImmediateResourceComponent, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use crate::config::{PG_POOL_SIZE, PgConnectConfig, PoolConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{
    LiveConfigOptions, LiveConfigSource, Store, impl_live_config, live_config_source_for,
};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db.connect]`、`[db.pool]`（还有开了 feature 才有的 `[db.live_config]`），各段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlxPgConfig {
    pub connect: PgConnectConfig,
    pub pool: PoolConfig,
    #[cfg(feature = "live-config")]
    pub live_config: LiveConfigOptions,
}

// ---- pool -------------------------------------------------------------------

/// PostgreSQL 连接池，`Deref` 到 sqlx 的 `PgPool`。用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct SqlxPgPool {
    inner: PgPool,
    #[cfg(feature = "live-config")]
    /// 配置里没开是 `None`
    live_config: Option<LiveConfigSource>,
}

#[async_trait]
impl ConfigResource for SqlxPgPool {
    type Config = SqlxPgConfig;

    async fn from_config(config: &SqlxPgConfig, app: &AppContext) -> Result<Self> {
        let target = config.connect.target();
        let options = config.connect.sqlx_options(app)?;
        #[cfg(feature = "live-config")]
        let live_config = live_config_source_for(
            |tables| Ok(Store::postgres(options.clone(), &config.pool, tables)),
            &config.live_config,
            app,
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
        Ok(Self {
            inner: pool,
            #[cfg(feature = "live-config")]
            live_config,
        })
    }

    /// 不再发新连接，等借出去的连接还回来（在途查询跑完）后全部关掉；动态配置的后台任务也退出
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
impl_live_config!(SqlxPgPool);

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
        let options = connect.sqlx_options(&AppContext::default()).unwrap();
        assert_eq!(options.get_options(), Some("-c search_path=public,custom"));
    }
}
