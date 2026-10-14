//! 连接池配置的公共部分：连哪个库（每种数据库一个）和池怎么管（所有数据库、框架共用）。
//! 各连接池的配置由它们拼起来，配置文件里是 `[db.connect]`、`[db.pool]` 两段

#[cfg(any(
    all(
        feature = "sqlx",
        any(feature = "postgres", feature = "sqlite", feature = "mysql")
    ),
    tokio_pg
))]
use std::time::Duration;

use serde::Deserialize;

#[cfg(any(
    feature = "postgres",
    all(any(feature = "sqlx", feature = "toasty"), feature = "mysql")
))]
use hygiea_core::app::RegistryConfig;

#[cfg(any(
    feature = "postgres",
    all(any(feature = "sqlx", feature = "toasty"), feature = "mysql"),
    sqlx_sqlite
))]
use hygiea_core::Result;

// ---- PostgreSQL -------------------------------------------------------------

/// 连哪个 PostgreSQL 库，sqlx、SeaORM 的连接池共用
#[cfg(feature = "postgres")]
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PgConnectConfig {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
    /// 连接 URL 的参数，不带开头的 `?`，比如 `sslmode=require`
    pub params: String,
    /// search_path，多个用逗号分隔；空的用数据库默认的
    pub schema_search_path: String,
}

#[cfg(feature = "postgres")]
impl Default for PgConnectConfig {
    fn default() -> Self {
        Self {
            scheme: "postgres".to_string(),
            host: "localhost".to_string(),
            port: 5432,
            username: "postgres".to_string(),
            password: "postgres".to_string(),
            database: "postgres".to_string(),
            params: String::new(),
            schema_search_path: String::new(),
        }
    }
}

#[cfg(feature = "postgres")]
impl PgConnectConfig {
    /// 连接 URL，不带 search_path（各框架另外设）。连接的 `application_name` 统一用 [`RegistryConfig::app_name`]
    /// （空的不设），`params` 里不能自己写，写了报 `AppErr::InvalidConfig`
    pub fn url(&self, registry_config: &RegistryConfig) -> Result<String> {
        use hygiea_core::app::AppErr;
        use hygiea_core::err;

        let mut params = self.params.trim_start_matches('?').to_string();
        let has_name = params
            .split('&')
            .any(|pair| pair.split('=').next() == Some("application_name"));
        if has_name {
            return Err(err!(
                AppErr::InvalidConfig,
                "connect.params: application_name comes from [registry] app_name, don't set it here"
            ));
        }
        let app_name = &registry_config.app_name;
        if !app_name.is_empty() {
            if !params.is_empty() {
                params.push('&');
            }
            params.push_str("application_name=");
            params.push_str(&percent_encode(app_name));
        }
        let query = if params.is_empty() {
            String::new()
        } else {
            format!("?{params}")
        };
        Ok(format!(
            "{}://{}:{}@{}:{}/{}{}",
            self.scheme, self.username, self.password, self.host, self.port, self.database, query
        ))
    }

    /// 日志、错误里用的连接目标，不带密码
    pub(crate) fn target(&self) -> String {
        format!(
            "postgres {}@{}:{}/{}",
            self.username, self.host, self.port, self.database
        )
    }

    /// tokio-postgres 的连接参数，带上 search_path 和 `application_name`（见 [`url`](Self::url)）
    #[cfg(tokio_pg)]
    pub(crate) fn tokio_pg_config(
        &self,
        registry_config: &RegistryConfig,
    ) -> Result<tokio_postgres::Config> {
        use hygiea_core::app::AppErr;
        use hygiea_core::{ResultExt, err};

        let mut config: tokio_postgres::Config = self
            .url(registry_config)?
            .parse()
            .wrap_err(|| err!(AppErr::InvalidConfig, "invalid postgres connection url"))?;
        if !self.schema_search_path.is_empty() {
            config.options(format!("-c search_path={}", self.schema_search_path));
        }
        Ok(config)
    }

    /// sqlx 的连接参数，带上 search_path 和 `application_name`（见 [`url`](Self::url)）
    #[cfg(any(feature = "sqlx", sqlx_live_config))]
    pub(crate) fn sqlx_options(
        &self,
        registry_config: &RegistryConfig,
    ) -> Result<sqlx::postgres::PgConnectOptions> {
        use hygiea_core::app::AppErr;
        use hygiea_core::{ResultExt, err};

        let options: sqlx::postgres::PgConnectOptions = self
            .url(registry_config)?
            .parse()
            .wrap_err(|| err!(AppErr::InvalidConfig, "invalid postgres connection url"))?;
        Ok(if self.schema_search_path.is_empty() {
            options
        } else {
            options.options([("search_path", self.schema_search_path.as_str())])
        })
    }
}

/// URL 参数值里的特殊字符转成 `%XX`（应用名正常只有 `[a-z0-9_-]`，不经过 Registry 时可能是别的）
#[cfg(feature = "postgres")]
fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---- MySQL ------------------------------------------------------------------

/// 连哪个 MySQL 库
#[cfg(all(any(feature = "sqlx", feature = "toasty"), feature = "mysql"))]
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MySqlConnectConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
    /// 连接 URL 的参数，不带开头的 `?`
    pub params: String,
}

#[cfg(all(any(feature = "sqlx", feature = "toasty"), feature = "mysql"))]
impl Default for MySqlConnectConfig {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 3306,
            username: "root".to_string(),
            password: String::new(),
            database: "mysql".to_string(),
            params: String::new(),
        }
    }
}

#[cfg(all(any(feature = "sqlx", feature = "toasty"), feature = "mysql"))]
impl MySqlConnectConfig {
    /// 连接 URL。`registry_config` 现在没用到，和 [`PgConnectConfig::url`] 保持一样的签名
    pub fn url(&self, _registry_config: &RegistryConfig) -> Result<String> {
        let params = self.params.trim_start_matches('?');
        let query = if params.is_empty() {
            String::new()
        } else {
            format!("?{params}")
        };
        Ok(format!(
            "mysql://{}:{}@{}:{}/{}{}",
            self.username, self.password, self.host, self.port, self.database, query
        ))
    }

    /// sqlx 的连接参数
    #[cfg(sqlx_mysql)]
    pub(crate) fn sqlx_options(
        &self,
        registry_config: &RegistryConfig,
    ) -> Result<sqlx::mysql::MySqlConnectOptions> {
        use hygiea_core::app::AppErr;
        use hygiea_core::{ResultExt, err};

        self.url(registry_config)?
            .parse()
            .wrap_err(|| err!(AppErr::InvalidConfig, "invalid mysql connection url"))
    }

    /// 日志、错误里用的连接目标，不带密码
    pub(crate) fn target(&self) -> String {
        format!(
            "mysql {}@{}:{}/{}",
            self.username, self.host, self.port, self.database
        )
    }
}

// ---- SQLite -----------------------------------------------------------------

/// 连哪个 SQLite 库，以及打开时的设置
#[cfg(all(feature = "sqlx", feature = "sqlite"))]
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqliteConnectConfig {
    /// 文件路径，`:memory:` 是内存库
    pub database: String,
    /// 连接 URL 的参数，不带开头的 `?`
    pub params: String,
    pub create_if_missing: bool,
    pub read_only: bool,
    pub foreign_keys: bool,
    pub shared_cache: bool,
    pub journal_mode: Option<String>,
    pub synchronous: Option<String>,
    pub busy_timeout_millis: Option<u64>,
    pub statement_cache_capacity: Option<usize>,
}

#[cfg(all(feature = "sqlx", feature = "sqlite"))]
impl Default for SqliteConnectConfig {
    fn default() -> Self {
        Self {
            database: "hygiea.db".to_string(),
            params: String::new(),
            create_if_missing: true,
            read_only: false,
            foreign_keys: true,
            shared_cache: false,
            journal_mode: Some("wal".to_string()),
            synchronous: Some("full".to_string()),
            busy_timeout_millis: Some(5_000),
            statement_cache_capacity: Some(100),
        }
    }
}

#[cfg(all(feature = "sqlx", feature = "sqlite"))]
impl SqliteConnectConfig {
    pub fn url(&self) -> String {
        let base = if self.database == ":memory:" {
            "sqlite::memory:".to_string()
        } else {
            format!("sqlite://{}", self.database)
        };
        let params = self.params.trim_start_matches('?');
        if params.is_empty() {
            base
        } else {
            format!("{base}?{params}")
        }
    }

    /// sqlx 的连接参数
    pub(crate) fn sqlx_options(&self) -> Result<sqlx::sqlite::SqliteConnectOptions> {
        use std::str::FromStr;

        use hygiea_core::app::AppErr;
        use hygiea_core::{ResultExt, err};
        use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};

        fn parse<T>(value: &str, name: &str) -> Result<T>
        where
            T: FromStr,
            T::Err: std::error::Error + Send + Sync + 'static,
        {
            value.parse().wrap_err(|| {
                err!(
                    AppErr::InvalidConfig,
                    format!("invalid SQLite {name}: {value}")
                )
            })
        }

        let mut options = SqliteConnectOptions::from_str(&self.url())
            .wrap_err(|| {
                err!(
                    AppErr::InvalidConfig,
                    format!("invalid SQLite database URL: {}", self.url())
                )
            })?
            .create_if_missing(self.create_if_missing)
            .read_only(self.read_only)
            .foreign_keys(self.foreign_keys)
            .shared_cache(self.shared_cache);
        if let Some(value) = &self.journal_mode {
            options = options.journal_mode(parse::<SqliteJournalMode>(value, "journal_mode")?);
        }
        if let Some(value) = &self.synchronous {
            options = options.synchronous(parse::<SqliteSynchronous>(value, "synchronous")?);
        }
        if let Some(value) = self.busy_timeout_millis {
            options = options.busy_timeout(Duration::from_millis(value));
        }
        if let Some(value) = self.statement_cache_capacity {
            options = options.statement_cache_capacity(value);
        }
        Ok(options)
    }
}

// ---- 连接池 -------------------------------------------------------------------

/// 池怎么管，和数据库、框架无关。数字项不写时按连接池给默认值：
/// 最大连接数 PG 10、SQLite 4，最小连接数 PG 0、SQLite 1；超时都是 acquire 30 秒、空闲 600 秒、最长存活 1800 秒
#[cfg(any(feature = "sqlx", feature = "seaorm", feature = "tokio-postgres"))]
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PoolConfig {
    pub max_connections: Option<u32>,
    pub min_connections: Option<u32>,
    /// 从池里拿连接最多等多久
    pub acquire_timeout_secs: Option<u64>,
    /// 空闲多久的连接关掉
    pub idle_timeout_secs: Option<u64>,
    /// 连接最长用多久，到了换新的
    pub max_lifetime_secs: Option<u64>,
    /// 拿连接前先检查还能不能用
    pub test_before_acquire: bool,
    /// 启动时不连，第一次用时才连
    pub connect_lazy: bool,
}

#[cfg(any(feature = "sqlx", feature = "seaorm", feature = "tokio-postgres"))]
impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: None,
            min_connections: None,
            acquire_timeout_secs: None,
            idle_timeout_secs: None,
            max_lifetime_secs: None,
            test_before_acquire: true,
            connect_lazy: false,
        }
    }
}

/// 连接池按后端给的默认连接数
#[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql, tokio_pg))]
#[derive(Clone, Copy)]
pub(crate) struct PoolSize {
    pub(crate) max: u32,
    /// deadpool（tokio-postgres）没有最小连接数，只有 sqlx 用
    #[cfg_attr(not(any(sqlx_pg, sqlx_sqlite, sqlx_mysql)), allow(dead_code))]
    pub(crate) min: u32,
}

#[cfg(any(sqlx_pg, tokio_pg))]
pub(crate) const PG_POOL_SIZE: PoolSize = PoolSize { max: 10, min: 0 };
#[cfg(sqlx_mysql)]
pub(crate) const MYSQL_POOL_SIZE: PoolSize = PoolSize { max: 10, min: 0 };
#[cfg(all(feature = "sqlx", feature = "sqlite"))]
pub(crate) const SQLITE_POOL_SIZE: PoolSize = PoolSize { max: 4, min: 1 };

#[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql, tokio_pg))]
impl PoolConfig {
    pub(crate) fn max_connections(&self, size: PoolSize) -> u32 {
        self.max_connections.unwrap_or(size.max)
    }

    #[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql))]
    pub(crate) fn min_connections(&self, size: PoolSize) -> u32 {
        self.min_connections.unwrap_or(size.min)
    }

    pub(crate) fn acquire_timeout_secs(&self) -> u64 {
        self.acquire_timeout_secs.unwrap_or(30)
    }

    #[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql))]
    pub(crate) fn idle_timeout_secs(&self) -> u64 {
        self.idle_timeout_secs.unwrap_or(600)
    }

    #[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql))]
    pub(crate) fn max_lifetime_secs(&self) -> u64 {
        self.max_lifetime_secs.unwrap_or(1800)
    }

    /// deadpool-postgres 的池参数：最大连接数、拿连接最多等多久。deadpool 没有最小连接数、空闲回收、最长存活，
    /// 这几项不生效；`test_before_acquire` 对应回收时检查连接（`RecyclingMethod::Fast` 只看连接断没断）
    #[cfg(tokio_pg)]
    pub(crate) fn deadpool(&self, size: PoolSize) -> deadpool_postgres::PoolConfig {
        let mut config = deadpool_postgres::PoolConfig::new(self.max_connections(size) as usize);
        config.timeouts.wait = Some(Duration::from_secs(self.acquire_timeout_secs()));
        config
    }

    /// sqlx 的池参数
    #[cfg(feature = "sqlx")]
    pub(crate) fn sqlx<DB: sqlx::Database>(&self, size: PoolSize) -> sqlx::pool::PoolOptions<DB> {
        sqlx::pool::PoolOptions::new()
            .max_connections(self.max_connections(size))
            .min_connections(self.min_connections(size))
            .acquire_timeout(Duration::from_secs(self.acquire_timeout_secs()))
            .idle_timeout(Duration::from_secs(self.idle_timeout_secs()))
            .max_lifetime(Duration::from_secs(self.max_lifetime_secs()))
            .test_before_acquire(self.test_before_acquire)
    }
}

#[cfg(all(
    test,
    any(
        feature = "postgres",
        all(any(feature = "sqlx", feature = "toasty"), feature = "mysql"),
        sqlx_sqlite
    )
))]
mod tests {
    use super::*;

    #[cfg(all(any(feature = "sqlx", feature = "toasty"), feature = "mysql"))]
    #[test]
    fn mysql_url() {
        let config = MySqlConnectConfig {
            password: "pw".to_string(),
            params: "?ssl-mode=required".to_string(),
            ..Default::default()
        };
        assert_eq!(
            config.url(&RegistryConfig::default()).unwrap(),
            "mysql://root:pw@localhost:3306/mysql?ssl-mode=required"
        );
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn pg_url() {
        let config = PgConnectConfig::default();
        let named_as = |app_name: &str| RegistryConfig {
            app_name: app_name.to_string(),
            ..Default::default()
        };
        let none = named_as("");
        assert_eq!(
            config.url(&none).unwrap(),
            "postgres://postgres:postgres@localhost:5432/postgres"
        );
        let config = PgConnectConfig {
            params: "sslmode=require".to_string(),
            schema_search_path: "app".to_string(),
            ..Default::default()
        };
        // search_path 不在 URL 里，各框架另外设
        assert_eq!(
            config.url(&none).unwrap(),
            "postgres://postgres:postgres@localhost:5432/postgres?sslmode=require"
        );
        assert_eq!(config.target(), "postgres postgres@localhost:5432/postgres");

        // 应用名：统一用 RegistryConfig 的，params 里不能自己写
        let app = named_as("dict-jp");
        assert_eq!(
            config.url(&app).unwrap(),
            "postgres://postgres:postgres@localhost:5432/postgres?sslmode=require&application_name=dict-jp"
        );
        let named = PgConnectConfig {
            params: "sslmode=require&application_name=replica".to_string(),
            ..Default::default()
        };
        assert!(named.url(&app).is_err());
        let odd = named_as("a b&c");
        assert!(
            PgConnectConfig::default()
                .url(&odd)
                .unwrap()
                .ends_with("?application_name=a%20b%26c")
        );
    }

    #[cfg(all(feature = "sqlx", feature = "sqlite"))]
    #[test]
    fn sqlite_url() {
        assert_eq!(SqliteConnectConfig::default().url(), "sqlite://hygiea.db");
        let memory = SqliteConnectConfig {
            database: ":memory:".to_string(),
            params: "?cache=shared".to_string(),
            ..Default::default()
        };
        assert_eq!(memory.url(), "sqlite::memory:?cache=shared");
        let file = SqliteConnectConfig {
            database: "/var/lib/app/data.db".to_string(),
            ..Default::default()
        };
        assert_eq!(file.url(), "sqlite:///var/lib/app/data.db");
    }

    #[cfg(all(feature = "sqlx", feature = "sqlite"))]
    #[test]
    fn rejects_invalid_sqlite_mode() {
        let config = SqliteConnectConfig {
            journal_mode: Some("not-a-mode".to_string()),
            ..Default::default()
        };
        assert!(config.sqlx_options().is_err());
    }

    /// 只写了一项时，其他项按后端的默认值，不会拿到别的后端的
    #[cfg(any(sqlx_pg, sqlx_sqlite))]
    #[test]
    fn pool_defaults_follow_backend() {
        let config = PoolConfig {
            acquire_timeout_secs: Some(5),
            ..Default::default()
        };
        let size = PoolSize { max: 4, min: 1 };
        assert_eq!(config.max_connections(size), 4);
        assert_eq!(config.min_connections(size), 1);
        assert_eq!(config.acquire_timeout_secs(), 5);
        assert_eq!(config.idle_timeout_secs(), 600);
    }
}
