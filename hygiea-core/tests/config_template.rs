//! 校验 `config/registry.toml`：按配置结构体解析（开了 deny_unknown_fields，写错或多写的配置项会解析失败），
//! 并确认模板里生效的值就是代码里的默认值。改了配置结构体，这个测试失败时要同步改模板。

#![cfg(feature = "app")]

use hygiea_core::app::RegistryConfig;
use hygiea_core::log::TracingConfig;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Template {
    registry: RegistryConfig,
}

fn read(file: &str) -> String {
    let path = format!("{}/config/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// 结构体没有实现 PartialEq，用 Debug 输出比较
fn same<T: std::fmt::Debug>(a: &T, b: &T) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

#[test]
fn registry_template_is_code_defaults() {
    let t: Template = toml::from_str(&read("registry.toml")).unwrap();
    let default = RegistryConfig::default();
    assert_eq!(t.registry.app_name, default.app_name);
    assert_eq!(
        t.registry.shutdown_timeout_secs,
        default.shutdown_timeout_secs
    );
    assert_eq!(t.registry.shutdown_delay_secs, default.shutdown_delay_secs);
    assert!(same(&t.registry.tracing, &TracingConfig::default()));
}
