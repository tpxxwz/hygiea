//! 本组测试共用的东西：通用工具从 hygiea-test-support 再导出，这里只放和 hygiea 类型相关的测试数据

use hygiea_core::net::http::ClientConfig;
use hygiea_core::redact::redact;
use serde::{Deserialize, Serialize};

pub use hygiea_test_support::headers::header_map;
pub use hygiea_test_support::http_server::*;
pub use hygiea_test_support::logs::capture;

/// 不走系统代理的配置：开发机上配了系统代理时，请求本地服务也可能被代理截走
pub fn local_config() -> ClientConfig {
    ClientConfig {
        no_proxy: true,
        ..ClientConfig::default()
    }
}

/// 带敏感字段的请求/响应体
#[redact]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Login {
    pub user: String,
    #[redact(mask)]
    pub token: String,
}

pub fn login(token: &str) -> Login {
    Login {
        user: "alice".into(),
        token: token.into(),
    }
}
