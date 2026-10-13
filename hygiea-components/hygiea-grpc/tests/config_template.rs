//! 校验 `config/tonic.toml`：按配置结构体解析（开了 deny_unknown_fields，写错或多写的配置项会解析失败），
//! 并确认模板里生效的值就是代码里的默认值。改了配置结构体，这个测试失败时要同步改模板。

#![cfg(feature = "tonic")]

use hygiea_grpc::TonicConfig;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Template {
    grpc: TonicConfig,
}

fn read(file: &str) -> String {
    let path = format!("{}/config/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// TonicConfig 里有建 router 的函数，不实现 Debug，逐项比较
#[test]
fn tonic_template_is_code_defaults() {
    let t: Template = toml::from_str(&read("tonic.toml")).unwrap();
    let default = TonicConfig::default();
    assert_eq!(t.grpc.addr, default.addr);
    assert_eq!(t.grpc.shutdown_timeout_secs, default.shutdown_timeout_secs);
}
