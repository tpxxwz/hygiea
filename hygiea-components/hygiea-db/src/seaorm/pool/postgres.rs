//! PostgreSQL 连接组件（SeaORM），feature `seaorm` + `postgres`

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use serde::Deserialize;

use hygiea_core::app::{AppErr, RegistryConfig, async_trait};
use hygiea_core::{Result, ResultExt, err};

use super::{SeaOrmLogConfig, close_connection, connect_options, pool_sections};
use crate::component::{DbComponent, DbConfig, DbUses, Sections};
use crate::config::{PG_POOL_SIZE, PgConnectConfig, PoolConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{DbLiveConfig, PgLiveConfigConfig, Store};
#[cfg(feature = "lock")]
use crate::lock::{DbLock, PgLock, PgLockConfig};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db]` 的 `uses`，加上 `[db.connect]`、`[db.pool]`、`[db.log]`（还有开了 feature 才有的 `[db.live_config]`、`[db.lock]`）。
/// `uses` 决定提供什么、哪些段能写，见 [`DbUses`]；能写的段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeaOrmPgConfig {
    /// 提供什么：1 业务池、2 动态配置、4 锁，按位加，不写是 1
    pub uses: DbUses,
    pub connect: PgConnectConfig,
    /// 业务池的参数，`uses` 带 1 时才能写
    pub pool: Option<PoolConfig>,
    /// 业务池的 SQL 日志，`uses` 带 1 时才能写
    pub log: Option<SeaOrmLogConfig>,
    /// 动态配置，`uses` 带 2 时才能写
    #[cfg(feature = "live-config")]
    pub live_config: Option<PgLiveConfigConfig>,
    /// 锁，`uses` 带 4 时才能写
    #[cfg(feature = "lock")]
    pub lock: Option<PgLockConfig>,
}

impl SeaOrmPgConfig {
    fn connect_options(&self, registry_config: &RegistryConfig) -> Result<ConnectOptions> {
        let mut opt = connect_options(
            self.connect.url(registry_config)?,
            &self.pool.clone().unwrap_or_default(),
            &self.log.clone().unwrap_or_default(),
            PG_POOL_SIZE,
        );
        if !self.connect.schema_search_path.is_empty() {
            opt.set_schema_search_path(&self.connect.schema_search_path);
        }
        Ok(opt)
    }
}

#[async_trait]
impl DbConfig for SeaOrmPgConfig {
    type Pool = SeaOrmPgPool;
    const LOCK: bool = true;

    #[allow(clippy::needless_update)] // feature 全开时各项都写了，`..Default::default()` 是给没开的 feature 补的
    fn sections(&self) -> Sections {
        Sections {
            uses: self.uses,
            pool: pool_sections(&self.pool, &self.log),
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

    async fn connect(&self, registry_config: &RegistryConfig) -> Result<SeaOrmPgPool> {
        let target = self.connect.target();
        tracing::info!("Connecting to database: {target}");
        let inner = Database::connect(self.connect_options(registry_config)?)
            .await
            .wrap_err(|| err!(AppErr::ConnectFailed, target.clone()))?;
        tracing::info!("Database connection established");
        Ok(SeaOrmPgPool { inner })
    }

    /// 等在途查询跑完后关掉连接池
    async fn close(pool: &SeaOrmPgPool) -> Result<()> {
        close_connection(&pool.inner).await
    }

    #[cfg(feature = "live-config")]
    fn live_config(&self, registry_config: &RegistryConfig) -> Result<DbLiveConfig> {
        let config = self.live_config.clone().unwrap_or_default();
        let pool = self.pool.clone().unwrap_or_default();
        DbLiveConfig::new(
            |tables| {
                let connect = config.connect.merge(&self.connect);
                Ok(Store::postgres(
                    connect.sqlx_options(registry_config)?,
                    &pool,
                    tables,
                ))
            },
            &config,
            registry_config,
        )
    }

    /// 和业务池共用时用 SeaORM 底下的 sqlx 池：`Database::connect` 按 postgres:// 建的，一定是 PG 连接池，
    /// `get_postgres_connection_pool` 不会 panic
    #[cfg(feature = "lock")]
    async fn lock(
        &self,
        pool: Option<&SeaOrmPgPool>,
        registry_config: &RegistryConfig,
    ) -> Result<DbLock> {
        let lock = PgLock::new(
            pool.map(|p| p.inner.get_postgres_connection_pool()),
            &self.connect,
            self.pool.as_ref(),
            self.lock.as_ref(),
            registry_config,
        )
        .await?;
        Ok(DbLock::postgres(lock))
    }
}

// ---- pool -------------------------------------------------------------------

/// PostgreSQL 连接（SeaORM），`Deref` 到 `DatabaseConnection`。`uses` 带 1 时按组件名放进 Resources；
/// 不经过 Registry 用 [`DbResources::from_config`](crate::DbResources::from_config) 建
#[derive(Clone, Debug)]
pub struct SeaOrmPgPool {
    inner: DatabaseConnection,
}

impl std::ops::Deref for SeaOrmPgPool {
    type Target = DatabaseConnection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// PostgreSQL 连接组件（SeaORM），按 `uses` 放进 Resources 的是 [`SeaOrmPgPool`]、`DbLiveConfig`、`DbLock`，见 [`DbComponent`]
pub type SeaOrmPgComponent = DbComponent<SeaOrmPgConfig>;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 配置模板 `config/seaorm-pg.toml` 就是代码里的默认值，加上 `uses = 7` 和写出来的各段（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(all(feature = "live-config", feature = "lock"))]
    #[test]
    fn template_is_code_defaults() {
        let config: SeaOrmPgConfig =
            crate::config::parse_template(include_str!("../../../config/seaorm-pg.toml"));
        let full = SeaOrmPgConfig {
            uses: DbUses::try_from(7).unwrap(),
            pool: Some(Default::default()),
            log: Some(Default::default()),
            live_config: Some(Default::default()),
            lock: Some(Default::default()),
            ..Default::default()
        };
        assert_eq!(format!("{config:?}"), format!("{full:?}"));
    }

    /// 按 `uses` 提供资源：业务池、动态配置按组件名，锁匿名
    #[test]
    fn provides_by_uses() {
        crate::component::check_provides(SeaOrmPgConfig::default(), |uses| SeaOrmPgConfig {
            uses,
            ..Default::default()
        });
    }

    /// 池参数不写时用 PG 的默认值，写了的按写的
    #[test]
    fn pool_options_applied() {
        let config = SeaOrmPgConfig::default();
        let options = config.connect_options(&RegistryConfig::default()).unwrap();
        assert_eq!(options.get_max_connections(), Some(10));
        let config = SeaOrmPgConfig {
            pool: Some(PoolConfig {
                max_connections: Some(3),
                ..Default::default()
            }),
            ..Default::default()
        };
        let options = config.connect_options(&RegistryConfig::default()).unwrap();
        assert_eq!(options.get_max_connections(), Some(3));
        assert_eq!(options.get_min_connections(), Some(0));
        assert_eq!(options.get_acquire_timeout(), Some(Duration::from_secs(30)));
    }
}
