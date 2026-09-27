//! 从配置文件（TOML）反序列化 `ReqwestConfig`：字段格式、默认值、非法值在什么时候报错

use std::time::Duration;

use hygiea_http_client::reqwest_client::*;

/// 空配置等于代码默认值
#[test]
fn empty_toml_uses_defaults() {
    let parsed: ReqwestConfig = toml::from_str("").unwrap();
    assert_eq!(
        format!("{parsed:?}"),
        format!("{:?}", ReqwestConfig::default())
    );
}

/// 拼错的字段名直接报错，不会被悄悄忽略
#[test]
fn unknown_field_is_rejected() {
    assert!(toml::from_str::<ReqwestConfig>("max_redirect = 0").is_err());
    assert!(
        toml::from_str::<ReqwestConfig>("[[proxies]]\nurl = \"http://x\"\nuser = \"u\"").is_err()
    );
}

/// 超时、resolve、代理这些字段转成运行时类型
#[test]
fn fields_convert_to_runtime_types() {
    let config: ReqwestConfig = toml::from_str(
        r#"
timeout = { secs = 5, nanos = 0 }
read_timeout = { secs = 7, nanos = 250000000 }
connect_timeout = { secs = 2, nanos = 0 }
max_redirects = 0
resolve = [["example.com", ["127.0.0.1:80"]]]
[default_headers]
X-Api-Key = "key"
[[proxies]]
kind = "http"
url = "http://127.0.0.1:7890"
"#,
    )
    .unwrap();
    assert_eq!(config.timeout, Some(Duration::from_secs(5)));
    assert_eq!(config.read_timeout, Some(Duration::new(7, 250_000_000)));
    assert_eq!(config.connect_timeout, Some(Duration::from_secs(2)));
    assert_eq!(config.max_redirects, 0);
    let headers = HeaderMap::try_from(config.default_headers.clone()).unwrap();
    assert_eq!(headers["x-api-key"], "key");
    assert_eq!(config.proxies.len(), 1);
    assert_eq!(config.resolve[0].0, "example.com");
    assert_eq!(config.resolve[0].1[0].to_string(), "127.0.0.1:80");
    assert!(config.build().is_ok());
}

/// 请求头的三种写法：字符串、同名头列表、带 sensitive 的文本或字节
#[test]
fn header_value_forms() {
    let config: ReqwestConfig = toml::from_str(
        r#"
[default_headers]
x-repeat = ["one", "two"]
authorization = { value = "Bearer secret", sensitive = true }
x-binary = { bytes = [128, 65], sensitive = true }
"#,
    )
    .unwrap();
    let headers = HeaderMap::try_from(config.default_headers).unwrap();
    let values: Vec<_> = headers.get_all("x-repeat").iter().collect();
    assert_eq!(values, ["one", "two"]);
    assert!(headers["authorization"].is_sensitive());
    assert_eq!(headers["x-binary"].as_bytes(), &[128, 65]);
    assert!(headers["x-binary"].is_sensitive());
}

/// 配置文件不校验保留头（`ReqwestConfig::default_headers` 方法才校验），原样保留
#[test]
fn reserved_header_is_kept() {
    let config: ReqwestConfig =
        toml::from_str("[default_headers]\ncontent-type = \"application/json\"").unwrap();
    let headers = HeaderMap::try_from(config.default_headers).unwrap();
    assert_eq!(headers["content-type"], "application/json");
}

/// 反序列化只管格式，非法的头名、头值、代理 URL 到 build 时才报错
mod rejected_when_building {
    use super::*;

    fn build_err(toml: &str) {
        let config: ReqwestConfig = toml::from_str(toml).unwrap();
        assert!(config.build().is_err(), "{toml}");
    }

    #[test]
    fn invalid_header_name() {
        build_err("[default_headers]\n\"bad name\" = \"x\"");
    }

    #[test]
    fn invalid_header_value() {
        build_err("[default_headers]\nx-key = \"bad\\nvalue\"");
    }

    #[test]
    fn invalid_proxy_url() {
        build_err("[[proxies]]\nurl = \"not a url\"");
    }

    #[test]
    fn invalid_proxy_header() {
        build_err(
            "[[proxies]]\nurl = \"http://127.0.0.1:7890\"\nheaders = { \"bad name\" = \"x\" }",
        );
    }
}
