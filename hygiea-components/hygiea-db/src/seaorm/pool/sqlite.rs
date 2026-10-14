//! SQLite 连接组件（SeaORM），feature `seaorm` + `sqlite`。SQLite 没有锁的实现，`uses` 只能带 1、2

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use serde::Deserialize;

use hygiea_core::app::{AppErr, RegistryConfig, async_trait};
use hygiea_core::{Result, ResultExt, err};

use super::{SeaOrmLogConfig, close_connection, connect_options, pool_sections};
use crate::component::{DbComponent, DbConfig, DbUses, Sections};
use crate::config::{PoolConfig, SQLITE_POOL_SIZE, SqliteConnectConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{DbLiveConfig, SqliteLiveConfigConfig, Store};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db]` 的 `uses`，加上 `[db.connect]`、`[db.pool]`、`[db.log]`（还有开了 feature 才有的 `[db.live_config]`）。
/// `uses` 决定提供什么、哪些段能写，见 [`DbUses`]；能写的段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeaOrmSqliteConfig {
    /// 提供什么：1 业务池、2 动态配置，按位加，不写是 1。SQLite 没有锁，带 4 启动报错
    pub uses: DbUses,
    pub connect: SqliteConnectConfig,
    /// 业务池的参数，`uses` 带 1 时才能写
    pub pool: Option<PoolConfig>,
    /// 业务池的 SQL 日志，`uses` 带 1 时才能写
    pub log: Option<SeaOrmLogConfig>,
    /// 动态配置，`uses` 带 2 时才能写。动态配置另开一条连接，`:memory:` 库（不共享缓存时）连到的是另一个空库，用不了
    #[cfg(feature = "live-config")]
    pub live_config: Option<SqliteLiveConfigConfig>,
}

impl SeaOrmSqliteConfig {
    /// URL、池参数、日志之外，`[db.connect]` 里的打开设置（journal_mode、foreign_keys ..）在 SeaORM 建 sqlx 的连接参数时加上
    fn connect_options(&self) -> Result<ConnectOptions> {
        let settings = self.connect.settings()?;
        let mut opt = connect_options(
            self.connect.url(),
            &self.pool.clone().unwrap_or_default(),
            &self.log.clone().unwrap_or_default(),
            SQLITE_POOL_SIZE,
        );
        opt.map_sqlx_sqlite_opts(move |options| settings.apply(options));
        Ok(opt)
    }
}

#[async_trait]
impl DbConfig for SeaOrmSqliteConfig {
    type Pool = SeaOrmSqlitePool;
    const LOCK: bool = false;

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
            ..Default::default()
        }
    }

    async fn connect(&self, _registry_config: &RegistryConfig) -> Result<SeaOrmSqlitePool> {
        tracing::info!("Connecting to SQLite database: {}", self.connect.database);
        let inner = Database::connect(self.connect_options()?)
            .await
            .wrap_err(|| {
                err!(
                    AppErr::ConnectFailed,
                    format!("sqlite {}", self.connect.database)
                )
            })?;
        tracing::info!("SQLite database connection established");
        Ok(SeaOrmSqlitePool { inner })
    }

    /// 等在途查询跑完后关掉连接池
    async fn close(pool: &SeaOrmSqlitePool) -> Result<()> {
        close_connection(&pool.inner).await
    }

    /// SQLite 没有连接名，应用名只拿来当表名前缀
    #[cfg(feature = "live-config")]
    fn live_config(&self, registry_config: &RegistryConfig) -> Result<DbLiveConfig> {
        let config = self.live_config.clone().unwrap_or_default();
        let pool = self.pool.clone().unwrap_or_default();
        DbLiveConfig::new(
            |tables| {
                let connect = config.connect.merge(&self.connect);
                Ok(Store::sqlite(connect.sqlx_options()?, &pool, tables))
            },
            &config,
            registry_config,
        )
    }
}

// ---- pool -------------------------------------------------------------------

/// SQLite 连接（SeaORM），`Deref` 到 `DatabaseConnection`。`uses` 带 1 时按组件名放进 Resources；
/// 不经过 Registry 用 [`DbResources::from_config`](crate::DbResources::from_config) 建
#[derive(Clone, Debug)]
pub struct SeaOrmSqlitePool {
    inner: DatabaseConnection,
}

impl std::ops::Deref for SeaOrmSqlitePool {
    type Target = DatabaseConnection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// SQLite 连接组件（SeaORM），按 `uses` 放进 Resources 的是 [`SeaOrmSqlitePool`]、`DbLiveConfig`，见 [`DbComponent`]
pub type SeaOrmSqliteComponent = DbComponent<SeaOrmSqliteConfig>;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 配置模板 `config/seaorm-sqlite.toml` 就是代码里的默认值，加上 `uses = 3` 和写出来的各段（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(feature = "live-config")]
    #[test]
    fn template_is_code_defaults() {
        let config: SeaOrmSqliteConfig =
            crate::config::parse_template(include_str!("../../../config/seaorm-sqlite.toml"));
        let full = SeaOrmSqliteConfig {
            uses: DbUses::try_from(3).unwrap(),
            pool: Some(Default::default()),
            log: Some(Default::default()),
            live_config: Some(Default::default()),
            ..Default::default()
        };
        assert_eq!(format!("{config:?}"), format!("{full:?}"));
    }

    /// 按 `uses` 提供资源：业务池、动态配置按组件名
    #[test]
    fn provides_by_uses() {
        crate::component::check_provides(SeaOrmSqliteConfig::default(), |uses| {
            SeaOrmSqliteConfig {
                uses,
                ..Default::default()
            }
        });
    }

    /// 池参数不写时用 SQLite 的默认值（最多 4 条、最少 1 条），写了的按写的；打开设置写错的启动时报错
    #[test]
    fn pool_options_applied() {
        let config = SeaOrmSqliteConfig::default();
        let options = config.connect_options().unwrap();
        assert_eq!(options.get_max_connections(), Some(4));
        assert_eq!(options.get_min_connections(), Some(1));
        assert_eq!(options.get_acquire_timeout(), Some(Duration::from_secs(30)));

        let mut bad = SeaOrmSqliteConfig::default();
        bad.connect.journal_mode = Some("not-a-mode".to_string());
        assert!(bad.connect_options().is_err());
    }
}
