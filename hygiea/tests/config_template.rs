//! 校验 docs/config-template.toml：按框架和各组件的配置结构体解析（都开了 deny_unknown_fields，
//! 写错或多写的配置项会解析失败），并确认模板里写的值就是代码里的默认值。
//! 改了任何配置结构体，这个测试失败时要同步改模板。
//!
//! 需要所有组件的 feature：`cargo test -p hygiea --all-features --test config_template`

#![cfg(all(
    feature = "app",
    feature = "db-sqlx-postgres",
    feature = "db-sqlx-sqlite",
    feature = "db-seaorm-postgres",
    feature = "redis-fred",
    feature = "http-axum",
    feature = "http-client-reqwest",
    feature = "grpc-tonic"
))]

use hygiea::app::RegistryConfig;
use hygiea::db::{SeaOrmPgConfig, SqlxPgConfig, SqlxSqliteConfig};
use hygiea::grpc::TonicConfig;
use hygiea::http::AxumConfig;
use hygiea::http_client::reqwest_client::ReqwestConfig;
use hygiea::log::TracingConfig;
use hygiea::redis::RedisConfig;
use serde::Deserialize;

/// 模板里的段名，对应应用自己 AppConfig 的字段名
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Template {
    registry: RegistryConfig,
    db_pg: SqlxPgConfig,
    db_seaorm: SeaOrmPgConfig,
    db_sqlite: SqlxSqliteConfig,
    redis: RedisConfig,
    http: AxumConfig,
    http_client: ReqwestConfig,
    grpc: TonicConfig,
}

fn template() -> Template {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/config-template.toml");
    let text = std::fs::read_to_string(path).unwrap();
    toml::from_str(&text).unwrap()
}

/// 结构体没有实现 PartialEq，用 Debug 输出比较
fn same<T: std::fmt::Debug>(a: &T, b: &T) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

#[test]
fn template_parses_into_all_configs() {
    template();
}

#[test]
fn component_values_are_code_defaults() {
    let t = template();
    assert!(same(&t.db_pg, &SqlxPgConfig::default()));
    assert!(same(&t.db_seaorm, &SeaOrmPgConfig::default()));
    assert!(same(&t.db_sqlite, &SqlxSqliteConfig::default()));
    assert!(same(&t.redis, &RedisConfig::default()));
    assert!(same(&t.http_client, &ReqwestConfig::default()));
    let grpc_default = TonicConfig::default();
    assert_eq!(t.grpc.addr, grpc_default.addr);
    assert_eq!(
        t.grpc.shutdown_timeout_secs,
        grpc_default.shutdown_timeout_secs
    );
}

#[test]
fn registry_values_are_code_defaults() {
    let t = template();
    let default = RegistryConfig::default();
    assert_eq!(
        t.registry.shutdown_timeout_secs,
        default.shutdown_timeout_secs
    );
    assert_eq!(t.registry.shutdown_delay_secs, default.shutdown_delay_secs);
    assert!(same(&t.registry.tracing, &TracingConfig::default()));
}

#[test]
fn http_values_are_code_defaults() {
    let t = template();
    let default = AxumConfig::default();
    assert!(same(&t.http, &default));
}
