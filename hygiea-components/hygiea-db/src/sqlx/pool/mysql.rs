//! MySQL 连接池组件（sqlx），feature `sqlx` + `mysql`

use serde::Deserialize;
use sqlx::MySqlPool;

use hygiea_core::app::{AppErr, RegistryConfig, async_trait};
use hygiea_core::{Result, ResultExt, err};

use crate::component::{DbComponent, DbConfig, DbUses, Sections};
use crate::config::{MYSQL_POOL_SIZE, MySqlConnectConfig, PoolConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{DbLiveConfig, MySqlLiveConfigConfig, Store};
#[cfg(feature = "lock")]
use crate::lock::{DbLockClient, MySqlLock, MySqlLockConfig};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db]` 的 `uses`，加上 `[db.connect]`、`[db.pool]`（还有开了 feature 才有的 `[db.live_config]`、`[db.lock]`）。
/// `uses` 决定提供什么、哪些段能写，见 [`DbUses`]；能写的段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlxMySqlConfig {
    /// 提供什么：1 业务池、2 动态配置、4 锁，按位加，不写是 1
    pub uses: DbUses,
    pub connect: MySqlConnectConfig,
    /// 业务池的参数，`uses` 带 1 时才能写
    pub pool: Option<PoolConfig>,
    /// 动态配置，`uses` 带 2 时才能写
    #[cfg(feature = "live-config")]
    pub live_config: Option<MySqlLiveConfigConfig>,
    /// 锁，`uses` 带 4 时才能写
    #[cfg(feature = "lock")]
    pub lock: Option<MySqlLockConfig>,
}

#[async_trait]
impl DbConfig for SqlxMySqlConfig {
    type Pool = SqlxMySqlPool;
    const LOCK: bool = true;

    #[allow(clippy::needless_update)] // feature 全开时各项都写了，`..Default::default()` 是给没开的 feature 补的
    fn sections(&self) -> Sections {
        Sections {
            uses: self.uses,
            pool: self.pool.iter().map(|_| "pool").collect(),
            #[cfg(feature = "live-config")]
            live_config: self.live_config.is_some(),
            #[cfg(feature = "live-config")]
            live_config_connect: self
                .live_config
                .as_ref()
                .is_some_and(|c| !c.connect.is_empty()),
            #[cfg(feature = "lock")]
            lock: self.lock.is_some(),
            #[cfg(feature = "lock")]
            lock_connect: self.lock.as_ref().is_some_and(|c| !c.connect.is_empty()),
            ..Default::default()
        }
    }

    async fn connect(&self, registry_config: &RegistryConfig) -> Result<SqlxMySqlPool> {
        let target = self.connect.target();
        let options = self.connect.sqlx_options(registry_config)?;
        let pool = self.pool.clone().unwrap_or_default();
        tracing::info!("Connecting to database: {target}");
        let pool_options = pool.sqlx::<sqlx::MySql>(MYSQL_POOL_SIZE);
        let inner = if pool.connect_lazy {
            pool_options.connect_lazy_with(options)
        } else {
            pool_options
                .connect_with(options)
                .await
                .wrap_err(|| err!(AppErr::ConnectFailed, target.clone()))?
        };
        tracing::info!("Database connection established");
        Ok(SqlxMySqlPool { inner })
    }

    /// 不再发新连接，等借出去的连接还回来（在途查询跑完）后全部关掉
    async fn close(pool: &SqlxMySqlPool) -> Result<()> {
        pool.inner.close().await;
        tracing::info!("Database connection pool closed");
        Ok(())
    }

    #[cfg(feature = "live-config")]
    fn live_config(&self, registry_config: &RegistryConfig) -> Result<DbLiveConfig> {
        let config = self.live_config.clone().unwrap_or_default();
        let pool = self.pool.clone().unwrap_or_default();
        DbLiveConfig::new(
            |tables| {
                let connect = config.connect.merge(&self.connect);
                Ok(Store::mysql(
                    connect.sqlx_options(registry_config)?,
                    &pool,
                    tables,
                ))
            },
            &config,
            registry_config,
        )
    }

    #[cfg(feature = "lock")]
    async fn lock_client(
        &self,
        pool: Option<&SqlxMySqlPool>,
        registry_config: &RegistryConfig,
    ) -> Result<DbLockClient> {
        let lock = MySqlLock::new(
            pool.map(|p| &p.inner),
            &self.connect,
            self.pool.as_ref(),
            self.lock.as_ref(),
            registry_config,
        )
        .await?;
        Ok(DbLockClient::mysql(lock))
    }
}

// ---- pool -------------------------------------------------------------------

/// MySQL 连接池，`Deref` 到 sqlx 的 `MySqlPool`。`uses` 带 1 时按组件名放进 Resources；
/// 不经过 Registry 用 [`DbResources::from_config`](crate::DbResources::from_config) 建
#[derive(Clone, Debug)]
pub struct SqlxMySqlPool {
    inner: MySqlPool,
}

impl std::ops::Deref for SqlxMySqlPool {
    type Target = MySqlPool;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// MySQL 连接池组件（sqlx），按 `uses` 放进 Resources 的是 [`SqlxMySqlPool`]、`DbLiveConfig`、`DbLockClient`，见 [`DbComponent`]
pub type SqlxMySqlComponent = DbComponent<SqlxMySqlConfig>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置模板 `config/sqlx-mysql.toml` 就是代码里的默认值，加上 `uses = 7` 和写出来的各段（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(all(feature = "live-config", feature = "lock"))]
    #[test]
    fn template_is_code_defaults() {
        let config: SqlxMySqlConfig =
            crate::config::parse_template(include_str!("../../../config/sqlx-mysql.toml"));
        let full = SqlxMySqlConfig {
            uses: DbUses::try_from(7).unwrap(),
            pool: Some(Default::default()),
            live_config: Some(Default::default()),
            lock: Some(Default::default()),
            ..Default::default()
        };
        assert_eq!(format!("{config:?}"), format!("{full:?}"));
    }

    /// 按 `uses` 提供资源：业务池、动态配置按组件名，锁匿名
    #[test]
    fn provides_by_uses() {
        crate::component::check_provides(SqlxMySqlConfig::default(), |uses| SqlxMySqlConfig {
            uses,
            ..Default::default()
        });
    }

    #[tokio::test]
    async fn connect_lazy_to_nonexistent_host_succeeds() {
        // 连不到的主机，connect_lazy 也能建出池子，第一次用时才连
        let mut config = SqlxMySqlConfig::default();
        config.connect.host = "nonexistent-host-xyz-12345.invalid".to_string();
        config.pool = Some(PoolConfig {
            connect_lazy: true,
            ..Default::default()
        });
        let db = crate::DbResources::from_config(&config, &Default::default())
            .await
            .unwrap();
        assert!(db.pool.is_some());
    }
}
