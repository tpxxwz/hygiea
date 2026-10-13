//! 校验 `config/aws.toml`：按配置结构体解析（开了 deny_unknown_fields，写错或多写的配置项会解析失败），
//! 并确认模板里生效的值就是代码里的默认值。改了配置结构体，这个测试失败时要同步改模板。

#![cfg(feature = "s3")]

use hygiea_aws::AwsConfig;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Template {
    aws: AwsConfig,
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
fn aws_template_is_code_defaults() {
    let t: Template = toml::from_str(&read("aws.toml")).unwrap();
    assert!(same(&t.aws, &AwsConfig::default()));
}
