//! SQLite 连接池组件（sqlx），feature `sqlx` + `sqlite`

use serde::Deserialize;
use sqlx::SqlitePool;

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, ResultExt, err};

use crate::config::{PoolConfig, SQLITE_POOL_SIZE, SqliteConnectConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{
    LiveConfigSource, SqliteLiveConfigConfig, Store, impl_live_config, live_config_source_for,
};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db.connect]`、`[db.pool]`（还有开了 feature 才有的 `[db.live_config]`），各段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlxSqliteConfig {
    pub connect: SqliteConnectConfig,
    pub pool: PoolConfig,
    /// 动态配置另开一条连接，`:memory:` 库（不共享缓存时）连到的是另一个空库，用不了
    #[cfg(feature = "live-config")]
    pub live_config: SqliteLiveConfigConfig,
}

// ---- pool -------------------------------------------------------------------

/// SQLite 连接池，`Deref` 到 sqlx 的 `SqlitePool`。用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct SqlxSqlitePool {
    inner: SqlitePool,
    #[cfg(feature = "live-config")]
    /// 配置里没开是 `None`
    live_config: Option<LiveConfigSource>,
}

#[async_trait]
impl ConfigResource for SqlxSqlitePool {
    type Config = SqlxSqliteConfig;

    async fn from_config(
        config: &SqlxSqliteConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        // SQLite 没有连接名，应用名只有动态配置拿来当表名前缀
        #[cfg(not(feature = "live-config"))]
        let _ = registry_config;
        tracing::info!("Connecting to SQLite database: {}", config.connect.database);
        let connect_options = config.connect.sqlx_options()?;
        #[cfg(feature = "live-config")]
        let live_config = live_config_source_for(
            |tables| {
                let connect = config.live_config.connect.merge(&config.connect);
                Ok(Store::sqlite(connect.sqlx_options()?, &config.pool, tables))
            },
            &config.live_config,
            registry_config,
        )?;
        let pool_options = config.pool.sqlx::<sqlx::Sqlite>(SQLITE_POOL_SIZE);
        let pool = if config.pool.connect_lazy {
            pool_options.connect_lazy_with(connect_options)
        } else {
            pool_options
                .connect_with(connect_options)
                .await
                .wrap_err(|| {
                    err!(
                        AppErr::ConnectFailed,
                        format!("sqlite {}", config.connect.database)
                    )
                })?
        };
        tracing::info!("SQLite database connection established");
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
        tracing::info!("SQLite connection pool closed");
        Ok(())
    }
}

#[cfg(feature = "live-config")]
impl_live_config!(SqlxSqlitePool);

impl std::ops::Deref for SqlxSqlitePool {
    type Target = SqlitePool;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// SQLite 连接池组件（sqlx），按组件名放进 Resources 的是 [`SqlxSqlitePool`]。用法见 [`ImmediateResourceComponent`]
pub type SqlxSqliteComponent = ImmediateResourceComponent<SqlxSqlitePool>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置模板 `config/sqlx-sqlite.toml` 就是代码里的默认值（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(feature = "live-config")]
    #[test]
    fn template_is_code_defaults() {
        let config: SqlxSqliteConfig =
            crate::config::parse_template(include_str!("../../../config/sqlx-sqlite.toml"));
        assert_eq!(
            format!("{config:?}"),
            format!("{:?}", SqlxSqliteConfig::default())
        );
    }

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        use hygiea_core::app::{Component, Name, ResourceId};

        let named = SqlxSqliteComponent::build(Name::from("primary"), SqlxSqliteConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<SqlxSqlitePool>("primary")]
        );
        let anonymous = SqlxSqliteComponent::build(Name::from(""), SqlxSqliteConfig::default());
        assert_eq!(
            anonymous.provides(),
            vec![ResourceId::of::<SqlxSqlitePool>()]
        );
    }

    fn memory(max_connections: u32) -> SqlxSqliteConfig {
        let mut config = SqlxSqliteConfig::default();
        config.connect.database = ":memory:".to_string();
        config.pool.max_connections = Some(max_connections);
        config
    }

    #[tokio::test]
    async fn connect_and_query_memory_database() {
        let pool = SqlxSqlitePool::from_config(&memory(1), &Default::default())
            .await
            .unwrap();

        sqlx::query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .execute(&*pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO items (name) VALUES (?)")
            .bind("hygiea")
            .execute(&*pool)
            .await
            .unwrap();

        let (name,): (String,) = sqlx::query_as("SELECT name FROM items WHERE id = ?")
            .bind(1_i64)
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(name, "hygiea");
    }

    #[tokio::test]
    async fn applies_common_sqlite_options() {
        let mut config = memory(1);
        config.connect.journal_mode = Some("memory".to_string());
        config.connect.synchronous = Some("normal".to_string());
        config.connect.busy_timeout_millis = Some(2_500);
        config.connect.statement_cache_capacity = Some(64);
        let pool = SqlxSqlitePool::from_config(&config, &Default::default())
            .await
            .unwrap();

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&*pool)
            .await
            .unwrap();
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&*pool)
            .await
            .unwrap();
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(journal_mode, "memory");
        assert_eq!(synchronous, 1);
        assert_eq!(busy_timeout, 2_500);
    }

    /// `create_if_missing` 生效：开着时文件库不存在会被建出来
    #[tokio::test]
    async fn create_if_missing_creates_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("new.db");
        let config = SqlxSqliteConfig {
            connect: SqliteConnectConfig {
                database: path.to_string_lossy().to_string(),
                create_if_missing: true,
                ..Default::default()
            },
            ..Default::default()
        };
        SqlxSqlitePool::from_config(&config, &Default::default())
            .await
            .unwrap();
        assert!(path.exists());
    }

    /// `create_if_missing` 关掉时，文件库不存在就连不上，也不会建文件
    #[tokio::test]
    async fn without_create_if_missing_missing_file_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("absent.db");
        let config = SqlxSqliteConfig {
            connect: SqliteConnectConfig {
                database: path.to_string_lossy().to_string(),
                create_if_missing: false,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(
            SqlxSqlitePool::from_config(&config, &Default::default())
                .await
                .is_err()
        );
        assert!(!path.exists());
    }
}
