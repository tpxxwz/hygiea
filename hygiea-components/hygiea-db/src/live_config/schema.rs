//! 动态配置的表：名字（前缀 + 固定的名字）、期望的结构、建表 DDL，以及第一次 bind 时的检查规则，和驱动无关
//! （查询、建表的执行在各驱动的实现里，现在是 `sqlx/<数据库>.rs`）。
//!
//! 检查表、函数、触发器时，只看 `current_schema()`（不带 schema 的 CREATE 落在这里）：
//!
//! - 一个都没有：建。在事务里建，PG 加 advisory lock（`hygiea_core::builtin` 的 `(LOCK_TYPE, Lock::LiveConfigDdl)`）、SQLite 用 `BEGIN IMMEDIATE`，
//!   拿到锁后再查一遍（多个实例同时启动，前一个可能已经建好了）
//! - 都在、表的列和代码一致：直接用
//! - 其他（只有一部分、列对不上）：不动库里的东西，拒绝启动。哪里不对和建表 DDL 打在日志里，手动处理
//!
//! # 改表、改触发器时要注意
//!
//! - 改 `sql/<数据库>/live_config.sql` 的表结构，要同时改同目录 `live_config.rs` 的 `COLUMNS`（列名、顺序、类型；PG 写 `udt_name`，
//!   比如 `int8`、`bpchar`，SQLite 写建表时的类型），否则已经建好的库启动时就报对不上
//! - 已经建好的库不会自动升级：框架只在"一个都没有"时建，结构变了要手动执行 DDL（ALTER TABLE 原地改，
//!   不要 DROP 重建，表里是线上配置）
//! - 改函数、触发器（`live_config.sql` 后半段）同样不会自动替换已有的，要手动执行。改的时候别动这三样，
//!   还在跑的旧版 worker 靠它们：NOTIFY 的 channel 名（`{p}config`）、payload 格式（`namespace:revision`）、
//!   revision 每次 +1 且按提交顺序递增（靠 `{p}config_revision` 那一行的行锁）
//! - PG 的函数都带 `SET search_path FROM CURRENT`，新加函数也要带：触发器在改表的那个会话里执行，
//!   不带的话 search_path 不一样的程序改表时找不到函数
//! - SQLite 测试里在不同连接上先 DROP 再 CREATE 同名对象会报"已存在"（别的连接缓存的 schema 是旧的），
//!   DDL 放在同一条连接上执行

#[cfg(any(live_config_sqlite, live_config_mysql))]
use serde_json::{Map, Value};

use hygiea_core::app::AppErr;
use hygiea_core::{HyErr, Result, ResultExt, err};

use super::Backend;
use crate::RepoErr;
#[cfg(live_config_mysql)]
use crate::sql::mysql::live_config as mysql;
#[cfg(live_config_pg)]
use crate::sql::postgres::live_config as pg;
#[cfg(live_config_sqlite)]
use crate::sql::sqlite::live_config as sqlite;
use crate::sql::{Columns, is_ident};

// ---- 名字、结构、DDL ---------------------------------------------------------------

/// 表名前缀是应用名加 `_`（`dict_jp` → `dict_jp_config`）。前缀拼进 SQL（表名不能绑参数），应用名得是
/// 小写下划线 `[a-z][a-z0-9_]*`：经过 Registry 时入口已经校验过，这里再查一次（不经过 Registry 时 RegistryConfig 是自己建的）。
/// 不能带 schema，表建在连接池 search_path 的第一个 schema 里
#[derive(Clone, Debug)]
pub(crate) struct Tables {
    prefix: String,
}

impl Tables {
    pub(crate) fn new(app_name: &str) -> Result<Self> {
        if !is_ident(app_name) {
            return Err(err!(
                AppErr::InvalidConfig,
                format!(
                    "live config: app name {app_name:?} is the table prefix, needs snake_case [a-z][a-z0-9_]*"
                )
            ));
        }
        Ok(Self {
            prefix: format!("{app_name}_"),
        })
    }

    pub(crate) fn config(&self) -> String {
        format!("{}config", self.prefix)
    }

    pub(crate) fn revision(&self) -> String {
        format!("{}config_revision", self.prefix)
    }

    pub(crate) fn history(&self) -> String {
        format!("{}config_history", self.prefix)
    }

    /// NOTIFY 的 channel，和配置表同名。只有能 LISTEN 的驱动（sqlx、tokio-postgres）用
    #[cfg(any(all(sqlx_live_config, sqlx_pg), tokio_pg_live_config))]
    pub(crate) fn channel(&self) -> String {
        self.config()
    }

    /// 三张表
    pub(crate) fn table_names(&self) -> [String; 3] {
        [self.config(), self.revision(), self.history()]
    }

    /// 期望的列：每张表按顺序的 (列名, 类型)。PG 的类型是 `udt_name`，SQLite 是建表时写的类型
    pub(crate) fn expected_columns(
        &self,
        backend: Backend,
    ) -> Vec<(String, Vec<(&'static str, &'static str)>)> {
        let columns = match backend {
            #[cfg(live_config_pg)]
            Backend::Postgres => pg::COLUMNS,
            #[cfg(live_config_sqlite)]
            Backend::Sqlite => sqlite::COLUMNS,
            #[cfg(live_config_mysql)]
            Backend::MySql => mysql::COLUMNS,
        };
        self.table_names()
            .into_iter()
            .zip(columns)
            .map(|(table, columns)| (table, columns.to_vec()))
            .collect()
    }

    /// 函数名（只有 PG 有）
    #[cfg(live_config_pg)]
    pub(crate) fn functions(&self) -> Vec<String> {
        self.prefixed(&pg::FUNCTIONS)
    }

    /// 触发器名
    pub(crate) fn triggers(&self, backend: Backend) -> Vec<String> {
        match backend {
            #[cfg(live_config_pg)]
            Backend::Postgres => self.prefixed(&pg::TRIGGERS),
            #[cfg(live_config_sqlite)]
            Backend::Sqlite => self.prefixed(&sqlite::TRIGGERS),
            #[cfg(live_config_mysql)]
            Backend::MySql => self.prefixed(&mysql::TRIGGERS),
        }
    }

    /// 建表、函数、触发器的语句：模板里的 `{p}` 换成前缀
    pub(crate) fn ddl(&self, backend: Backend) -> String {
        let template = match backend {
            #[cfg(live_config_pg)]
            Backend::Postgres => pg::DDL,
            #[cfg(live_config_sqlite)]
            Backend::Sqlite => sqlite::DDL,
            #[cfg(live_config_mysql)]
            Backend::MySql => mysql::DDL,
        };
        template.replace("{p}", &self.prefix)
    }

    fn prefixed(&self, names: &[&str]) -> Vec<String> {
        names
            .iter()
            .map(|name| format!("{}{name}", self.prefix))
            .collect()
    }
}

/// 渲染好的查询，建 `Store` 时拼一次。SQL 在 `sql/<数据库>/live_config.rs`
pub(crate) struct Sql {
    pub(crate) load: String,
    pub(crate) revisions: String,
    pub(crate) seed: String,
    /// 变更历史只留最新的 N 行，参数是 N
    pub(crate) trim_history: String,
}

impl Sql {
    pub(crate) fn new(tables: &Tables, backend: Backend) -> Self {
        let (config, revision, history) = (tables.config(), tables.revision(), tables.history());
        match backend {
            #[cfg(live_config_pg)]
            Backend::Postgres => Self {
                load: pg::load(&config, &revision),
                revisions: pg::revisions(&revision),
                seed: pg::seed(&config),
                trim_history: pg::trim_history(&history),
            },
            #[cfg(live_config_sqlite)]
            Backend::Sqlite => Self {
                load: sqlite::load(&config, &revision),
                revisions: sqlite::revisions(&revision),
                seed: sqlite::seed(&config),
                trim_history: sqlite::trim_history(&history),
            },
            #[cfg(live_config_mysql)]
            Backend::MySql => Self {
                load: mysql::load(&config, &revision),
                revisions: mysql::revisions(&revision),
                seed: mysql::seed(&config),
                trim_history: mysql::trim_history(&history),
            },
        }
    }
}

/// 补默认值时按行拼参数的库（SQLite、MySQL）：(namespace, key, value 的 JSON 文本)
#[cfg(any(live_config_sqlite, live_config_mysql))]
pub(crate) fn seed_rows(defaults: &[(String, Map<String, Value>)]) -> Vec<(&str, &str, String)> {
    defaults
        .iter()
        .flat_map(|(namespace, values)| {
            values
                .iter()
                .map(move |(key, value)| (namespace.as_str(), key.as_str(), value.to_string()))
        })
        .collect()
}

// ---- 检查的规则 -------------------------------------------------------------------

pub(crate) enum State {
    Absent,
    Ready,
    /// 每个不对的地方一行
    Broken(Vec<String>),
}

/// `objects` 是库里已有的同名函数、触发器
pub(crate) fn state(
    tables: &Tables,
    backend: Backend,
    columns: &Columns,
    expected_objects: &[String],
    objects: &[String],
) -> State {
    if columns.is_empty() && objects.is_empty() {
        return State::Absent;
    }
    let mut problems = Vec::new();
    for (table, expected) in tables.expected_columns(backend) {
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(name, ty)| (name.to_string(), ty.to_string()))
            .collect();
        match columns.get(&table) {
            None => problems.push(format!("table {table} missing")),
            Some(actual) if *actual != expected => problems.push(format!(
                "table {table} columns: expected {expected:?}, actual {actual:?}"
            )),
            Some(_) => {}
        }
    }
    for name in expected_objects {
        if !objects.contains(name) {
            problems.push(format!("function / trigger {name} missing"));
        }
    }
    if problems.is_empty() {
        State::Ready
    } else {
        State::Broken(problems)
    }
}

/// 拒绝启动：日志里打出哪里不对和建表 DDL
pub(crate) fn broken(problems: &[String], tables: &Tables, backend: Backend) -> Result<()> {
    for problem in problems {
        tracing::error!(
            problem,
            "live config: tables are not as expected, left untouched"
        );
    }
    tracing::error!(
        "live config: fix them by hand (or use another app name) to match this DDL, then restart:\n{}",
        tables.ddl(backend)
    );
    Err(err!(RepoErr::LiveConfigSchemaMismatch, tables.config()))
}

/// 建表失败（多半是没有 DDL 权限）：日志里打出要手动执行的 DDL
pub(crate) fn created<E: std::error::Error + Send + Sync + 'static>(
    result: std::result::Result<(), E>,
    tables: &Tables,
    backend: Backend,
) -> Result<()> {
    if result.is_err() {
        tracing::error!(
            "live config: create failed, run this DDL manually:\n{}",
            tables.ddl(backend)
        );
    }
    result.wrap_err(|| err!(RepoErr::LiveConfigCreateFailed, tables.config()))?;
    tracing::info!(table = tables.config(), "live config: tables created");
    Ok(())
}

pub(crate) fn check_err() -> HyErr {
    err!(RepoErr::QueryFailed, "live config check tables")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(live_config_pg)]
    #[test]
    fn postgres_ddl() {
        let tables = Tables::new("app").unwrap();
        assert_eq!(tables.config(), "app_config");
        assert_eq!(
            tables.triggers(Backend::Postgres)[0],
            "app_config_on_change"
        );
        let ddl = tables.ddl(Backend::Postgres);
        assert!(ddl.contains("CREATE TABLE app_config_revision"));
        assert!(ddl.contains("CREATE TRIGGER app_config_on_change"));
        assert!(ddl.contains("pg_notify('app_config'"));
        assert!(!ddl.contains('{'), "占位符都要替换掉");
    }

    #[cfg(live_config_sqlite)]
    #[test]
    fn sqlite_ddl() {
        let tables = Tables::new("hy").unwrap();
        let ddl = tables.ddl(Backend::Sqlite);
        assert!(ddl.contains("CREATE TRIGGER hy_config_rename"));
        assert!(!ddl.contains('{'));
    }

    #[test]
    fn rejects_unsafe_app_name() {
        for app_name in [
            "", "Hy", "hy-test", "1hy", "_hy", "hy;drop", "app.hy", "hy test",
        ] {
            assert!(Tables::new(app_name).is_err(), "应该拒绝 {app_name:?}");
        }
        assert_eq!(Tables::new("dict_jp").unwrap().config(), "dict_jp_config");
    }
}

#[cfg(all(test, live_config_mysql))]
mod mysql_tests {
    use super::*;

    #[test]
    fn mysql_ddl() {
        let tables = Tables::new("hy").unwrap();
        let ddl = tables.ddl(Backend::MySql);
        assert!(ddl.contains("CREATE TRIGGER hy_config_update"));
        assert!(!ddl.contains('{'));
    }
}
