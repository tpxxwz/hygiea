//! 本组测试共用的东西：通用工具从 test-support 再导出，这里只放和 hygiea 类型相关的测试数据

use std::error::Error as _;

use hygiea_core::redact::redact;
use hygiea_http_client::reqwest_client::ReqwestConfig;
use serde::{Deserialize, Serialize};

pub use hygiea_core::app::ConfigResource;
pub use test_support::headers::header_map;
pub use test_support::http_server::*;
pub use test_support::logs::capture;

/// 错误的 source 是不是 reqwest 报的超时（`client_config.rs` / `send.rs` 共用）
pub fn is_timeout(err: &hygiea_core::HyErr) -> bool {
    err.source()
        .and_then(|e| e.downcast_ref::<reqwest::Error>())
        .is_some_and(|e| e.is_timeout())
}

/// 从 HyErr 上取原始的 reqwest::Error，传输层失败的测试里常用
pub fn reqwest_source(err: &hygiea_core::HyErr) -> &reqwest::Error {
    err.source().unwrap().downcast_ref().unwrap()
}

/// 不走系统代理的配置：开发机上配了系统代理时，请求本地服务也可能被代理截走
pub fn local_config() -> ReqwestConfig {
    ReqwestConfig {
        no_proxy: true,
        ..ReqwestConfig::default()
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
