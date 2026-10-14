//! SQLite 连接池组件（sqlx），feature `sqlx` + `sqlite`。SQLite 没有锁的实现，`uses` 只能带 1、2

use serde::Deserialize;
use sqlx::SqlitePool;

use hygiea_core::app::{AppErr, RegistryConfig, async_trait};
use hygiea_core::{Result, ResultExt, err};

use crate::component::{DbComponent, DbConfig, DbUses, Sections};
use crate::config::{PoolConfig, SQLITE_POOL_SIZE, SqliteConnectConfig};
#[cfg(feature = "live-config")]
use crate::live_config::{DbLiveConfig, SqliteLiveConfigConfig, Store};

// ---- config -----------------------------------------------------------------

/// 配置文件里是 `[db]` 的 `uses`，加上 `[db.connect]`、`[db.pool]`（还有开了 feature 才有的 `[db.live_config]`）。
/// `uses` 决定提供什么、哪些段能写，见 [`DbUses`]；能写的段不写用默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqlxSqliteConfig {
    /// 提供什么：1 业务池、2 动态配置，按位加，不写是 1。SQLite 没有锁，带 4 启动报错
    pub uses: DbUses,
    pub connect: SqliteConnectConfig,
    /// 业务池的参数，`uses` 带 1 时才能写
    pub pool: Option<PoolConfig>,
    /// 动态配置，`uses` 带 2 时才能写。动态配置另开一条连接，`:memory:` 库（不共享缓存时）连到的是另一个空库，用不了
    #[cfg(feature = "live-config")]
    pub live_config: Option<SqliteLiveConfigConfig>,
}

#[async_trait]
impl DbConfig for SqlxSqliteConfig {
    type Pool = SqlxSqlitePool;
    const LOCK: bool = false;

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
            ..Default::default()
        }
    }

    async fn connect(&self, _registry_config: &RegistryConfig) -> Result<SqlxSqlitePool> {
        tracing::info!("Connecting to SQLite database: {}", self.connect.database);
        let connect_options = self.connect.sqlx_options()?;
        let pool = self.pool.clone().unwrap_or_default();
        let pool_options = pool.sqlx::<sqlx::Sqlite>(SQLITE_POOL_SIZE);
        let inner = if pool.connect_lazy {
            pool_options.connect_lazy_with(connect_options)
        } else {
            pool_options
                .connect_with(connect_options)
                .await
                .wrap_err(|| {
                    err!(
                        AppErr::ConnectFailed,
                        format!("sqlite {}", self.connect.database)
                    )
                })?
        };
        tracing::info!("SQLite database connection established");
        Ok(SqlxSqlitePool { inner })
    }

    /// 不再发新连接，等借出去的连接还回来（在途查询跑完）后全部关掉
    async fn close(pool: &SqlxSqlitePool) -> Result<()> {
        pool.inner.close().await;
        tracing::info!("SQLite connection pool closed");
        Ok(())
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

/// SQLite 连接池，`Deref` 到 sqlx 的 `SqlitePool`。`uses` 带 1 时按组件名放进 Resources；
/// 不经过 Registry 用 [`DbResources::from_config`](crate::DbResources::from_config) 建
#[derive(Clone, Debug)]
pub struct SqlxSqlitePool {
    inner: SqlitePool,
}

impl std::ops::Deref for SqlxSqlitePool {
    type Target = SqlitePool;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ---- component --------------------------------------------------------------

/// SQLite 连接池组件（sqlx），按 `uses` 放进 Resources 的是 [`SqlxSqlitePool`]、`DbLiveConfig`，见 [`DbComponent`]
pub type SqlxSqliteComponent = DbComponent<SqlxSqliteConfig>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置模板 `config/sqlx-sqlite.toml` 就是代码里的默认值，加上 `uses = 3` 和写出来的各段（结构体没有实现 PartialEq，比 Debug 输出）
    #[cfg(feature = "live-config")]
    #[test]
    fn template_is_code_defaults() {
        let config: SqlxSqliteConfig =
            crate::config::parse_template(include_str!("../../../config/sqlx-sqlite.toml"));
        let full = SqlxSqliteConfig {
            uses: DbUses::try_from(3).unwrap(),
            pool: Some(Default::default()),
            live_config: Some(Default::default()),
            ..Default::default()
        };
        assert_eq!(format!("{config:?}"), format!("{full:?}"));
    }

    /// 按 `uses` 提供资源：业务池、动态配置按组件名
    #[test]
    fn provides_by_uses() {
        crate::component::check_provides(SqlxSqliteConfig::default(), |uses| SqlxSqliteConfig {
            uses,
            ..Default::default()
        });
    }

    /// 建业务池，不经过 Registry
    async fn open(config: &SqlxSqliteConfig) -> Result<SqlxSqlitePool> {
        let db = crate::DbResources::from_config(config, &Default::default()).await?;
        Ok(db.pool.unwrap())
    }

    fn memory(max_connections: u32) -> SqlxSqliteConfig {
        let mut config = SqlxSqliteConfig::default();
        config.connect.database = ":memory:".to_string();
        config.pool = Some(PoolConfig {
            max_connections: Some(max_connections),
            ..Default::default()
        });
        config
    }

    #[tokio::test]
    async fn connect_and_query_memory_database() {
        let pool = open(&memory(1)).await.unwrap();

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
        let pool = open(&config).await.unwrap();

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
        open(&config).await.unwrap();
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
        assert!(open(&config).await.is_err());
        assert!(!path.exists());
    }
}
