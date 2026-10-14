//! 连接池配置的公共部分：连哪个库（每种数据库一个）和池怎么管（所有数据库、框架共用）。
//! 各连接池的配置由它们拼起来，配置文件里是 `[db.connect]`、`[db.pool]` 两段

#[cfg(any(
    all(feature = "sqlx", any(feature = "postgres", feature = "mysql")),
    sqlx_sqlite
))]
use std::time::Duration;

use serde::Deserialize;

#[cfg(any(feature = "postgres", feature = "mysql"))]
use hygiea_core::app::RegistryConfig;

#[cfg(any(feature = "postgres", feature = "mysql", sqlx_sqlite))]
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

    /// sqlx 的连接参数，带上 search_path 和 `application_name`（见 [`url`](Self::url)）
    #[cfg(any(feature = "sqlx", sqlx_live_config, sqlx_lock_pg))]
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

/// 动态配置、锁单独连的库（`[db.live_config.connect]`、`[db.lock.connect]`）：每项都可以不写，不写的沿用
/// `[db.connect]`，只写不一样的（比如换个用户、schema）。只有 `uses` 带 1（有业务池）时能写，没有业务池时直接写在 `[db.connect]` 里
#[cfg(all(feature = "postgres", any(live_config_pg, lock_pg)))]
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PgConnectOverride {
    pub scheme: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub params: Option<String>,
    pub schema_search_path: Option<String>,
}

#[cfg(all(feature = "postgres", any(live_config_pg, lock_pg)))]
impl PgConnectOverride {
    /// 一项都没写
    pub(crate) fn is_empty(&self) -> bool {
        let Self {
            scheme,
            host,
            port,
            username,
            password,
            database,
            params,
            schema_search_path,
        } = self;
        scheme.is_none()
            && host.is_none()
            && port.is_none()
            && username.is_none()
            && password.is_none()
            && database.is_none()
            && params.is_none()
            && schema_search_path.is_none()
    }

    /// 写了的项盖到 `main` 上
    pub(crate) fn merge(&self, main: &PgConnectConfig) -> PgConnectConfig {
        let pick =
            |value: &Option<String>, main: &String| value.clone().unwrap_or_else(|| main.clone());
        PgConnectConfig {
            scheme: pick(&self.scheme, &main.scheme),
            host: pick(&self.host, &main.host),
            port: self.port.unwrap_or(main.port),
            username: pick(&self.username, &main.username),
            password: pick(&self.password, &main.password),
            database: pick(&self.database, &main.database),
            params: pick(&self.params, &main.params),
            schema_search_path: pick(&self.schema_search_path, &main.schema_search_path),
        }
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

/// 连哪个 MySQL 库，sqlx、SeaORM 的连接池共用
#[cfg(feature = "mysql")]
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MySqlConnectConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
    /// 连接 URL 的参数，不带开头的 `?`，比如 `ssl-mode=required`
    pub params: String,
}

#[cfg(feature = "mysql")]
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

#[cfg(feature = "mysql")]
impl MySqlConnectConfig {
    /// 连接 URL。MySQL 没有 PG 那样的 `application_name`，`registry_config` 现在没用到，签名和 PG 的保持一样
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

    /// 日志、错误里用的连接目标，不带密码
    pub(crate) fn target(&self) -> String {
        format!(
            "mysql {}@{}:{}/{}",
            self.username, self.host, self.port, self.database
        )
    }

    /// sqlx 的连接参数（sqlx 的连接池、动态配置的小池、锁单独的池用；只开 SeaORM 时用不到）
    #[cfg(any(feature = "sqlx", live_config_mysql, lock_mysql))]
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
}

/// 动态配置、锁单独连的库（`[db.live_config.connect]`、`[db.lock.connect]`）：每项都可以不写，不写的沿用
/// `[db.connect]`，只写不一样的（比如换个用户、库）。只有 `uses` 带 1（有业务池）时能写，没有业务池时直接写在 `[db.connect]` 里
#[cfg(any(live_config_mysql, lock_mysql))]
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MySqlConnectOverride {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub params: Option<String>,
}

#[cfg(any(live_config_mysql, lock_mysql))]
impl MySqlConnectOverride {
    /// 一项都没写
    pub(crate) fn is_empty(&self) -> bool {
        let Self {
            host,
            port,
            username,
            password,
            database,
            params,
        } = self;
        host.is_none()
            && port.is_none()
            && username.is_none()
            && password.is_none()
            && database.is_none()
            && params.is_none()
    }

    /// 写了的项盖到 `main` 上
    pub(crate) fn merge(&self, main: &MySqlConnectConfig) -> MySqlConnectConfig {
        let pick =
            |value: &Option<String>, main: &String| value.clone().unwrap_or_else(|| main.clone());
        MySqlConnectConfig {
            host: pick(&self.host, &main.host),
            port: self.port.unwrap_or(main.port),
            username: pick(&self.username, &main.username),
            password: pick(&self.password, &main.password),
            database: pick(&self.database, &main.database),
            params: pick(&self.params, &main.params),
        }
    }
}

// ---- SQLite -----------------------------------------------------------------

/// 连哪个 SQLite 库，以及打开时的设置，sqlx、SeaORM 的连接池共用
#[cfg(sqlx_sqlite)]
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

#[cfg(sqlx_sqlite)]
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

#[cfg(sqlx_sqlite)]
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

    /// sqlx 的连接参数：URL 加上 [`settings`](Self::settings)（sqlx 的连接池、动态配置的小池用；只开 SeaORM 时用不到）
    #[cfg(any(feature = "sqlx", live_config_sqlite))]
    pub(crate) fn sqlx_options(&self) -> Result<sqlx::sqlite::SqliteConnectOptions> {
        use std::str::FromStr;

        use hygiea_core::app::AppErr;
        use hygiea_core::{ResultExt, err};

        let options = sqlx::sqlite::SqliteConnectOptions::from_str(&self.url()).wrap_err(|| {
            err!(
                AppErr::InvalidConfig,
                format!("invalid SQLite database URL: {}", self.url())
            )
        })?;
        Ok(self.settings()?.apply(options))
    }

    /// URL 以外的打开设置，先解析（写错的在这里报错），再用 [`SqliteSettings::apply`] 加到连接参数上。
    /// SeaORM 只收 URL，要在它建连接参数时用回调加上，回调不能返回错误，所以分成两步
    pub(crate) fn settings(&self) -> Result<SqliteSettings> {
        use std::str::FromStr;

        use hygiea_core::app::AppErr;
        use hygiea_core::{ResultExt, err};

        fn parse<T>(value: &Option<String>, name: &str) -> Result<Option<T>>
        where
            T: FromStr,
            T::Err: std::error::Error + Send + Sync + 'static,
        {
            value
                .as_deref()
                .map(|value| {
                    value.parse().wrap_err(|| {
                        err!(
                            AppErr::InvalidConfig,
                            format!("invalid SQLite {name}: {value}")
                        )
                    })
                })
                .transpose()
        }

        Ok(SqliteSettings {
            create_if_missing: self.create_if_missing,
            read_only: self.read_only,
            foreign_keys: self.foreign_keys,
            shared_cache: self.shared_cache,
            journal_mode: parse(&self.journal_mode, "journal_mode")?,
            synchronous: parse(&self.synchronous, "synchronous")?,
            busy_timeout: self.busy_timeout_millis.map(Duration::from_millis),
            statement_cache_capacity: self.statement_cache_capacity,
        })
    }
}

/// 解析好的 SQLite 打开设置，见 [`SqliteConnectConfig::settings`]
#[cfg(sqlx_sqlite)]
#[derive(Clone, Debug)]
pub(crate) struct SqliteSettings {
    create_if_missing: bool,
    read_only: bool,
    foreign_keys: bool,
    shared_cache: bool,
    journal_mode: Option<sqlx::sqlite::SqliteJournalMode>,
    synchronous: Option<sqlx::sqlite::SqliteSynchronous>,
    busy_timeout: Option<Duration>,
    statement_cache_capacity: Option<usize>,
}

#[cfg(sqlx_sqlite)]
impl SqliteSettings {
    pub(crate) fn apply(
        &self,
        options: sqlx::sqlite::SqliteConnectOptions,
    ) -> sqlx::sqlite::SqliteConnectOptions {
        let mut options = options
            .create_if_missing(self.create_if_missing)
            .read_only(self.read_only)
            .foreign_keys(self.foreign_keys)
            .shared_cache(self.shared_cache);
        if let Some(value) = self.journal_mode {
            options = options.journal_mode(value);
        }
        if let Some(value) = self.synchronous {
            options = options.synchronous(value);
        }
        if let Some(value) = self.busy_timeout {
            options = options.busy_timeout(value);
        }
        if let Some(value) = self.statement_cache_capacity {
            options = options.statement_cache_capacity(value);
        }
        options
    }
}

/// 动态配置单独连的库（`[db.live_config.connect]`）：每项都可以不写，不写的沿用 `[db.connect]`。只有 `uses` 带 1（有业务池）时能写
#[cfg(live_config_sqlite)]
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SqliteConnectOverride {
    pub database: Option<String>,
    pub params: Option<String>,
    pub create_if_missing: Option<bool>,
    pub read_only: Option<bool>,
    pub foreign_keys: Option<bool>,
    pub shared_cache: Option<bool>,
    pub journal_mode: Option<String>,
    pub synchronous: Option<String>,
    pub busy_timeout_millis: Option<u64>,
    pub statement_cache_capacity: Option<usize>,
}

#[cfg(live_config_sqlite)]
impl SqliteConnectOverride {
    /// 一项都没写
    pub(crate) fn is_empty(&self) -> bool {
        let Self {
            database,
            params,
            create_if_missing,
            read_only,
            foreign_keys,
            shared_cache,
            journal_mode,
            synchronous,
            busy_timeout_millis,
            statement_cache_capacity,
        } = self;
        database.is_none()
            && params.is_none()
            && create_if_missing.is_none()
            && read_only.is_none()
            && foreign_keys.is_none()
            && shared_cache.is_none()
            && journal_mode.is_none()
            && synchronous.is_none()
            && busy_timeout_millis.is_none()
            && statement_cache_capacity.is_none()
    }

    /// 写了的项盖到 `main` 上
    pub(crate) fn merge(&self, main: &SqliteConnectConfig) -> SqliteConnectConfig {
        SqliteConnectConfig {
            database: self
                .database
                .clone()
                .unwrap_or_else(|| main.database.clone()),
            params: self.params.clone().unwrap_or_else(|| main.params.clone()),
            create_if_missing: self.create_if_missing.unwrap_or(main.create_if_missing),
            read_only: self.read_only.unwrap_or(main.read_only),
            foreign_keys: self.foreign_keys.unwrap_or(main.foreign_keys),
            shared_cache: self.shared_cache.unwrap_or(main.shared_cache),
            journal_mode: self
                .journal_mode
                .clone()
                .or_else(|| main.journal_mode.clone()),
            synchronous: self
                .synchronous
                .clone()
                .or_else(|| main.synchronous.clone()),
            busy_timeout_millis: self.busy_timeout_millis.or(main.busy_timeout_millis),
            statement_cache_capacity: self
                .statement_cache_capacity
                .or(main.statement_cache_capacity),
        }
    }
}

// ---- 连接池 -------------------------------------------------------------------

/// 池怎么管，和数据库、框架无关。数字项不写时按连接池给默认值：
/// 最大连接数 PG、MySQL 10，SQLite 4，最小连接数 PG、MySQL 0，SQLite 1；超时都是 acquire 30 秒、空闲 600 秒、最长存活 1800 秒
#[cfg(any(feature = "sqlx", feature = "seaorm"))]
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

#[cfg(any(feature = "sqlx", feature = "seaorm"))]
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
#[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql))]
#[derive(Clone, Copy)]
pub(crate) struct PoolSize {
    pub(crate) max: u32,
    pub(crate) min: u32,
}

#[cfg(sqlx_pg)]
pub(crate) const PG_POOL_SIZE: PoolSize = PoolSize { max: 10, min: 0 };
#[cfg(sqlx_mysql)]
pub(crate) const MYSQL_POOL_SIZE: PoolSize = PoolSize { max: 10, min: 0 };
#[cfg(sqlx_sqlite)]
pub(crate) const SQLITE_POOL_SIZE: PoolSize = PoolSize { max: 4, min: 1 };

#[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql))]
impl PoolConfig {
    pub(crate) fn max_connections(&self, size: PoolSize) -> u32 {
        self.max_connections.unwrap_or(size.max)
    }

    pub(crate) fn min_connections(&self, size: PoolSize) -> u32 {
        self.min_connections.unwrap_or(size.min)
    }

    pub(crate) fn acquire_timeout_secs(&self) -> u64 {
        self.acquire_timeout_secs.unwrap_or(30)
    }

    pub(crate) fn idle_timeout_secs(&self) -> u64 {
        self.idle_timeout_secs.unwrap_or(600)
    }

    pub(crate) fn max_lifetime_secs(&self) -> u64 {
        self.max_lifetime_secs.unwrap_or(1800)
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

/// 测试用：解析 `config/` 下的配置模板（段名是 `db`，写错或多写的配置项解析失败）。
/// 条件和用它的测试（各连接池的 `template_is_code_defaults`）一致：模板里都有 `[db.live_config]` 段，要开 `live-config`；
/// PG、MySQL 的模板还有 `[db.lock]` 段，要再开 `lock`
#[cfg(all(
    test,
    feature = "live-config",
    any(all(feature = "lock", any(sqlx_pg, sqlx_mysql)), sqlx_sqlite)
))]
pub(crate) fn parse_template<T: serde::de::DeserializeOwned>(template: &str) -> T {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Template<T> {
        db: T,
    }
    let parsed: Template<T> = toml::from_str(template).unwrap();
    parsed.db
}

#[cfg(all(test, any(feature = "postgres", feature = "mysql", sqlx_sqlite)))]
mod tests {
    use super::*;

    #[cfg(feature = "mysql")]
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
        assert_eq!(config.target(), "mysql root@localhost:3306/mysql");
    }

    /// 写了的项覆盖，没写的沿用连接池的
    #[cfg(any(live_config_mysql, lock_mysql))]
    #[test]
    fn mysql_override_merges_per_field() {
        let main = MySqlConnectConfig {
            host: "mysql-main".to_string(),
            username: "app".to_string(),
            ..Default::default()
        };
        let merged = MySqlConnectOverride {
            username: Some("app_lock".to_string()),
            database: Some("coord".to_string()),
            ..Default::default()
        }
        .merge(&main);
        assert_eq!(merged.host, "mysql-main");
        assert_eq!(merged.username, "app_lock");
        assert_eq!(merged.database, "coord");
        assert_eq!(merged.port, 3306);
    }

    #[cfg(sqlx_sqlite)]
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

    #[cfg(sqlx_sqlite)]
    #[test]
    fn rejects_invalid_sqlite_mode() {
        let config = SqliteConnectConfig {
            journal_mode: Some("not-a-mode".to_string()),
            ..Default::default()
        };
        assert!(config.settings().is_err());
    }

    /// 写了的项覆盖，没写的沿用连接池的
    #[cfg(any(live_config_pg, lock_pg))]
    #[test]
    fn pg_override_merges_per_field() {
        let main = PgConnectConfig {
            host: "pg-main".to_string(),
            username: "app".to_string(),
            schema_search_path: "biz".to_string(),
            ..Default::default()
        };
        let merged = PgConnectOverride {
            username: Some("app_config".to_string()),
            schema_search_path: Some(String::new()),
            ..Default::default()
        }
        .merge(&main);
        assert_eq!(merged.host, "pg-main");
        assert_eq!(merged.username, "app_config");
        assert_eq!(merged.schema_search_path, "", "写成空的也算写了");
        assert_eq!(merged.port, 5432);
    }

    #[cfg(live_config_sqlite)]
    #[test]
    fn sqlite_override_merges_per_field() {
        let main = SqliteConnectConfig {
            database: "main.db".to_string(),
            read_only: true,
            ..Default::default()
        };
        let merged = SqliteConnectOverride {
            database: Some("config.db".to_string()),
            ..Default::default()
        }
        .merge(&main);
        assert_eq!(merged.database, "config.db");
        assert!(merged.read_only);
    }

    /// 只写了一项时，其他项按后端的默认值，不会拿到别的后端的
    #[cfg(any(sqlx_pg, sqlx_sqlite, sqlx_mysql))]
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
