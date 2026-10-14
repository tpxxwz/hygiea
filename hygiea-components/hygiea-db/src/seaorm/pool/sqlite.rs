//! SQLite 连接组件（SeaORM），feature `seaorm` + `sqlite`。SQLite 没有锁的实现，只有动态配置

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use serde::Deserialize;

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use super::{SeaOrmLogConfig, connect_options};
use crate::config::{PoolConfig, SQLITE_POOL_SIZE, SqliteConnectConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{
    LiveConfigSource, SqliteLiveConfigConfig, Store, impl_live_config, live_config_source_for,
};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db.connect]`、`[db.pool]`、`[db.log]`（还有开了 feature 才有的 `[db.live_config]`），各段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeaOrmSqliteConfig {
    pub connect: SqliteConnectConfig,
    pub pool: PoolConfig,
    pub log: SeaOrmLogConfig,
    /// 动态配置另开一条连接，`:memory:` 库（不共享缓存时）连到的是另一个空库，用不了
    #[cfg(feature = "live-config")]
    pub live_config: SqliteLiveConfigConfig,
}

impl SeaOrmSqliteConfig {
    /// URL、池参数、日志之外，`[db.connect]` 里的打开设置（journal_mode、foreign_keys ..）在 SeaORM 建 sqlx 的连接参数时加上
    fn connect_options(&self) -> Result<ConnectOptions> {
        let settings = self.connect.settings()?;
        let mut opt = connect_options(self.connect.url(), &self.pool, &self.log, SQLITE_POOL_SIZE);
        opt.map_sqlx_sqlite_opts(move |options| settings.apply(options));
        Ok(opt)
    }
}

// ---- pool -------------------------------------------------------------------

/// SQLite 连接（SeaORM），`Deref` 到 `DatabaseConnection`。用 [`ConfigResource::from_config`] 建，
/// 不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct SeaOrmSqlitePool {
    inner: DatabaseConnection,
    #[cfg(feature = "live-config")]
    /// 配置里没开是 `None`
    live_config: Option<LiveConfigSource>,
}

#[async_trait]
impl ConfigResource for SeaOrmSqlitePool {
    type Config = SeaOrmSqliteConfig;

    async fn from_config(
        config: &SeaOrmSqliteConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        // SQLite 没有连接名，应用名只有动态配置拿来当表名前缀
        #[cfg(not(feature = "live-config"))]
        let _ = registry_config;
        #[cfg(feature = "live-config")]
        let live_config = live_config_source_for(
            |tables| {
                let connect = config.live_config.connect.merge(&config.connect);
                Ok(Store::sqlite(connect.sqlx_options()?, &config.pool, tables))
            },
            &config.live_config,
            registry_config,
        )?;
        tracing::info!("Connecting to SQLite database: {}", config.connect.database);
        let conn = Database::connect(config.connect_options()?)
            .await
            .wrap_err(|| {
                err!(
                    AppErr::ConnectFailed,
                    format!("sqlite {}", config.connect.database)
                )
            })?;
        tracing::info!("SQLite database connection established");
        Ok(Self {
            inner: conn,
            #[cfg(feature = "live-config")]
            live_config,
        })
    }

    /// 等在途查询跑完后关掉连接池；动态配置的后台任务也退出
    async fn close(&self) -> Result<()> {
        #[cfg(feature = "live-config")]
        if let Some(live_config) = &self.live_config {
            live_config.close().await;
        }
        self.inner
            .close_by_ref()
            .await
            .wrap_err(|| err!(AppErr::StopFailed, "close database connection"))?;
        tracing::info!("SQLite connection pool closed");
        Ok(())
    }
}

#[cfg(feature = "live-config")]
impl_live_config!(SeaOrmSqlitePool);

impl std::ops::Deref for SeaOrmSqlitePool {
    type Target = DatabaseConnection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// SQLite 连接组件（SeaORM），按组件名放进 Resources 的是 [`SeaOrmSqlitePool`]。用法见 [`ImmediateResourceComponent`]
pub type SeaOrmSqliteComponent = ImmediateResourceComponent<SeaOrmSqlitePool>;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 配置模板 `config/seaorm-sqlite.toml` 就是代码里的默认值（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(feature = "live-config")]
    #[test]
    fn template_is_code_defaults() {
        let config: SeaOrmSqliteConfig =
            crate::config::parse_template(include_str!("../../../config/seaorm-sqlite.toml"));
        assert_eq!(
            format!("{config:?}"),
            format!("{:?}", SeaOrmSqliteConfig::default())
        );
    }

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        use hygiea_core::app::{Component, Name, ResourceId};

        let named =
            SeaOrmSqliteComponent::build(Name::from("primary"), SeaOrmSqliteConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<SeaOrmSqlitePool>("primary")]
        );
        let anonymous = SeaOrmSqliteComponent::build(Name::from(""), SeaOrmSqliteConfig::default());
        assert_eq!(
            anonymous.provides(),
            vec![ResourceId::of::<SeaOrmSqlitePool>()]
        );
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
