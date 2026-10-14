//! 校验 `config/` 下各连接池的模板：按配置结构体解析（开了 deny_unknown_fields，写错或多写的配置项会解析失败），
//! 并确认模板里生效的值就是代码里的默认值。改了配置结构体，这个测试失败时要同步改模板。
//! 模板里有 `[db.live_config]` 段，要开 `live-config`：`cargo test -p hygiea-db --all-features --test config_template`

#![cfg(any(
    all(
        feature = "live-config",
        any(
            all(feature = "sqlx", feature = "postgres"),
            all(feature = "seaorm", feature = "postgres"),
            all(feature = "sqlx", feature = "sqlite")
        )
    ),
    all(feature = "live-config", feature = "sqlx", feature = "mysql"),
    all(
        feature = "live-config",
        feature = "tokio-postgres",
        feature = "postgres"
    ),
    all(feature = "live-config", feature = "toasty", feature = "postgres"),
    all(feature = "toasty", feature = "mysql")
))]

use serde::Deserialize;
use serde::de::DeserializeOwned;

/// 模板里的段名是 `db`
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Template<T> {
    db: T,
}

fn parse<T: DeserializeOwned>(file: &str) -> T {
    let t: Template<T> = toml::from_str(&read(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
    t.db
}

fn read(file: &str) -> String {
    let path = format!("{}/config/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// 结构体没有实现 PartialEq，用 Debug 输出比较
fn same<T: std::fmt::Debug>(a: &T, b: &T) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

#[cfg(all(feature = "live-config", feature = "sqlx", feature = "postgres"))]
#[test]
fn sqlx_pg_template_is_code_defaults() {
    let config: hygiea_db::SqlxPgConfig = parse("sqlx-pg.toml");
    assert!(same(&config, &hygiea_db::SqlxPgConfig::default()));
}

#[cfg(all(feature = "live-config", feature = "seaorm", feature = "postgres"))]
#[test]
fn seaorm_pg_template_is_code_defaults() {
    let config: hygiea_db::SeaOrmPgConfig = parse("seaorm-pg.toml");
    assert!(same(&config, &hygiea_db::SeaOrmPgConfig::default()));
}

#[cfg(all(feature = "live-config", feature = "sqlx", feature = "sqlite"))]
#[test]
fn sqlx_sqlite_template_is_code_defaults() {
    let config: hygiea_db::SqlxSqliteConfig = parse("sqlx-sqlite.toml");
    assert!(same(&config, &hygiea_db::SqlxSqliteConfig::default()));
}

#[cfg(all(feature = "live-config", feature = "toasty", feature = "postgres"))]
#[test]
fn toasty_pg_template_is_code_defaults() {
    let config: hygiea_db::ToastyPgConfig = parse("toasty-pg.toml");
    assert!(same(&config, &hygiea_db::ToastyPgConfig::default()));
}

#[cfg(all(feature = "toasty", feature = "mysql"))]
#[test]
fn toasty_mysql_template_is_code_defaults() {
    let config: hygiea_db::ToastyMySqlConfig = parse("toasty-mysql.toml");
    assert!(same(&config, &hygiea_db::ToastyMySqlConfig::default()));
}

#[cfg(all(feature = "live-config", feature = "sqlx", feature = "mysql"))]
#[test]
fn sqlx_mysql_template_is_code_defaults() {
    let config: hygiea_db::SqlxMySqlConfig = parse("sqlx-mysql.toml");
    assert!(same(&config, &hygiea_db::SqlxMySqlConfig::default()));
}

#[cfg(all(
    feature = "live-config",
    feature = "tokio-postgres",
    feature = "postgres"
))]
#[test]
fn tokio_pg_template_is_code_defaults() {
    let config: hygiea_db::TokioPgConfig = parse("tokio-pg.toml");
    assert!(same(&config, &hygiea_db::TokioPgConfig::default()));
}
