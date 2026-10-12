//! 动态配置的表：名字（前缀 + 固定的名字）、期望的结构、建表 DDL，以及第一次 bind 时的检查。
//!
//! 检查表、函数、触发器时，只看 `current_schema()`（不带 schema 的 CREATE 落在这里）：
//!
//! - 一个都没有：建。在事务里建，PG 加 advisory lock、SQLite 用 `BEGIN IMMEDIATE`，
//!   拿到锁后再查一遍（多个实例同时启动，前一个可能已经建好了）
//! - 都在、表的列和代码一致：直接用
//! - 其他（只有一部分、列对不上）：不动库里的东西，拒绝启动。哪里不对和建表 DDL 打在日志里，手动处理
//!
//! # 改表、改触发器时要注意
//!
//! - 改 `sql/*.sql` 的表结构，要同时改 [`Tables::expected_columns`]（列名、顺序、类型；PG 写 `udt_name`，
//!   比如 `int8`、`bpchar`，SQLite 写建表时的类型），否则已经建好的库启动时就报对不上
//! - 已经建好的库不会自动升级：框架只在"一个都没有"时建，结构变了要手动执行 DDL（ALTER TABLE 原地改，
//!   不要 DROP 重建，表里是线上配置）
//! - 改函数、触发器（`sql/*.sql` 后半段）同样不会自动替换已有的，要手动执行。改的时候别动这三样，
//!   还在跑的旧版 worker 靠它们：NOTIFY 的 channel 名（`{p}config`）、payload 格式（`namespace:revision`）、
//!   revision 每次 +1 且按提交顺序递增（靠 `{p}config_revision` 那一行的行锁）
//! - PG 的函数都带 `SET search_path FROM CURRENT`，新加函数也要带：触发器在改表的那个会话里执行，
//!   不带的话 search_path 不一样的程序改表时找不到函数
//! - SQLite 测试里在不同连接上先 DROP 再 CREATE 同名对象会报"已存在"（别的连接缓存的 schema 是旧的），
//!   DDL 放在同一条连接上执行

use std::collections::HashMap;

#[cfg(feature = "postgres")]
use sqlx::PgConnection;
#[cfg(feature = "sqlite")]
use sqlx::SqliteConnection;
use sqlx::{AssertSqlSafe, Connection};

use hygiea_core::app::AppErr;
use hygiea_core::{HyErr, Result, ResultExt, err};

use super::Backend;
use crate::RepoErr;

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

    /// NOTIFY 的 channel，和配置表同名
    #[cfg(feature = "postgres")]
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
        let [config, revision, history] = self.table_names();
        match backend {
            #[cfg(feature = "postgres")]
            Backend::Postgres => vec![
                (
                    config,
                    vec![
                        ("namespace", "text"),
                        ("key", "text"),
                        ("value", "jsonb"),
                        ("updated_at", "timestamptz"),
                    ],
                ),
                (revision, vec![("namespace", "text"), ("revision", "int8")]),
                (
                    history,
                    vec![
                        ("id", "int8"),
                        ("namespace", "text"),
                        ("key", "text"),
                        ("op", "bpchar"),
                        ("old_value", "jsonb"),
                        ("new_value", "jsonb"),
                        ("revision", "int8"),
                        ("changed_at", "timestamptz"),
                        ("changed_by", "text"),
                    ],
                ),
            ],
            #[cfg(feature = "sqlite")]
            Backend::Sqlite => vec![
                (
                    config,
                    vec![
                        ("namespace", "TEXT"),
                        ("key", "TEXT"),
                        ("value", "TEXT"),
                        ("updated_at", "TEXT"),
                    ],
                ),
                (
                    revision,
                    vec![("namespace", "TEXT"), ("revision", "INTEGER")],
                ),
                (
                    history,
                    vec![
                        ("id", "INTEGER"),
                        ("namespace", "TEXT"),
                        ("key", "TEXT"),
                        ("op", "TEXT"),
                        ("old_value", "TEXT"),
                        ("new_value", "TEXT"),
                        ("revision", "INTEGER"),
                        ("changed_at", "TEXT"),
                    ],
                ),
            ],
        }
    }

    /// 函数名（只有 PG 有）
    #[cfg(feature = "postgres")]
    pub(crate) fn functions(&self) -> Vec<String> {
        ["config_record", "config_on_change", "config_touch"]
            .iter()
            .map(|name| format!("{}{name}", self.prefix))
            .collect()
    }

    /// 触发器名
    pub(crate) fn triggers(&self, backend: Backend) -> Vec<String> {
        let names: &[&str] = match backend {
            #[cfg(feature = "postgres")]
            Backend::Postgres => &["config_on_change", "config_touch"],
            #[cfg(feature = "sqlite")]
            Backend::Sqlite => &[
                "config_insert",
                "config_delete",
                "config_update",
                "config_rename",
            ],
        };
        names
            .iter()
            .map(|name| format!("{}{name}", self.prefix))
            .collect()
    }

    /// 建表、函数、触发器的语句：模板里的 `{p}` 换成前缀
    pub(crate) fn ddl(&self, backend: Backend) -> String {
        let template = match backend {
            #[cfg(feature = "postgres")]
            Backend::Postgres => include_str!("sql/postgres.sql"),
            #[cfg(feature = "sqlite")]
            Backend::Sqlite => include_str!("sql/sqlite.sql"),
        };
        template.replace("{p}", &self.prefix)
    }
}

/// `[a-z][a-z0-9_]*`
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

// ---- 检查、建表 -------------------------------------------------------------------

/// 三张表现在的样子：表名 → 按顺序的 (列名, 类型)，不在的表没有这一项
type Columns = HashMap<String, Vec<(String, String)>>;

enum State {
    Absent,
    Ready,
    /// 每个不对的地方一行
    Broken(Vec<String>),
}

/// `objects` 是库里已有的同名函数、触发器
fn state(
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
fn broken(problems: &[String], tables: &Tables, backend: Backend) -> Result<()> {
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
fn created(result: sqlx::Result<()>, tables: &Tables, backend: Backend) -> Result<()> {
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

fn check_err() -> HyErr {
    err!(RepoErr::QueryFailed, "live config check tables")
}

// ---- PostgreSQL --------------------------------------------------------------

#[cfg(feature = "postgres")]
pub(crate) async fn ensure_postgres(conn: &mut PgConnection, tables: &Tables) -> Result<()> {
    let backend = Backend::Postgres;
    let mut expected = tables.functions();
    expected.extend(tables.triggers(backend));
    let check =
        |columns: &Columns, objects: &[String]| state(tables, backend, columns, &expected, objects);

    let columns = postgres_columns(conn, tables).await.wrap_err(check_err)?;
    let objects = postgres_objects(conn, tables).await.wrap_err(check_err)?;
    match check(&columns, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }

    let mut tx = conn.begin().await.wrap_err(check_err)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(advisory_key(&tables.config()))
        .execute(&mut *tx)
        .await
        .wrap_err(check_err)?;
    let columns = postgres_columns(&mut tx, tables)
        .await
        .wrap_err(check_err)?;
    let objects = postgres_objects(&mut tx, tables)
        .await
        .wrap_err(check_err)?;
    match check(&columns, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }
    let result = async {
        sqlx::raw_sql(AssertSqlSafe(tables.ddl(backend)))
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    created(result, tables, backend)
}

#[cfg(feature = "postgres")]
async fn postgres_columns(conn: &mut PgConnection, tables: &Tables) -> sqlx::Result<Columns> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text, udt_name::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = ANY($1) \
         ORDER BY table_name, ordinal_position",
    )
    .bind(tables.table_names().to_vec())
    .fetch_all(&mut *conn)
    .await?;
    let mut columns = Columns::new();
    for (table, column, ty) in rows {
        columns.entry(table).or_default().push((column, ty));
    }
    Ok(columns)
}

/// 库里已有的同名函数、触发器（触发器挂在哪张表上都算）
#[cfg(feature = "postgres")]
async fn postgres_objects(conn: &mut PgConnection, tables: &Tables) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT p.proname::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
         WHERE n.nspname = current_schema() AND p.proname = ANY($1) \
         UNION ALL \
         SELECT t.tgname::text FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = current_schema() AND t.tgname = ANY($2) AND NOT t.tgisinternal",
    )
    .bind(tables.functions())
    .bind(tables.triggers(Backend::Postgres))
    .fetch_all(&mut *conn)
    .await
}

/// advisory lock 的 key：按配置表名算的 FNV-1a，同一个应用名（前缀）的实例互斥，不同的互不影响
#[cfg(feature = "postgres")]
fn advisory_key(name: &str) -> i64 {
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    hash as i64
}

// ---- SQLite ------------------------------------------------------------------

#[cfg(feature = "sqlite")]
pub(crate) async fn ensure_sqlite(conn: &mut SqliteConnection, tables: &Tables) -> Result<()> {
    let backend = Backend::Sqlite;
    let expected = tables.triggers(backend);
    let check =
        |columns: &Columns, objects: &[String]| state(tables, backend, columns, &expected, objects);

    let columns = sqlite_columns(conn, tables).await.wrap_err(check_err)?;
    let objects = sqlite_objects(conn, &expected).await.wrap_err(check_err)?;
    match check(&columns, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }

    // IMMEDIATE：开事务就拿写锁，别的进程同时建表要等这边提交
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .wrap_err(check_err)?;
    let columns = sqlite_columns(&mut tx, tables).await.wrap_err(check_err)?;
    let objects = sqlite_objects(&mut tx, &expected)
        .await
        .wrap_err(check_err)?;
    match check(&columns, &objects) {
        State::Ready => return Ok(()),
        State::Broken(problems) => return broken(&problems, tables, backend),
        State::Absent => {}
    }
    let result = async {
        sqlx::raw_sql(AssertSqlSafe(tables.ddl(backend)))
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    created(result, tables, backend)
}

#[cfg(feature = "sqlite")]
async fn sqlite_columns(conn: &mut SqliteConnection, tables: &Tables) -> sqlx::Result<Columns> {
    let mut columns = Columns::new();
    for table in tables.table_names() {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name, type FROM pragma_table_info(?1) ORDER BY cid")
                .bind(&table)
                .fetch_all(&mut *conn)
                .await?;
        if !rows.is_empty() {
            columns.insert(table, rows);
        }
    }
    Ok(columns)
}

/// 库里已有的同名触发器（挂在哪张表上都算）
#[cfg(feature = "sqlite")]
async fn sqlite_objects(
    conn: &mut SqliteConnection,
    expected: &[String],
) -> sqlx::Result<Vec<String>> {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'trigger'")
            .fetch_all(&mut *conn)
            .await?;
    Ok(names
        .into_iter()
        .filter(|name| expected.contains(name))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "postgres")]
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

    #[cfg(feature = "sqlite")]
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
