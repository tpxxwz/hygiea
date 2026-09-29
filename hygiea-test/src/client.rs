//! 测试用的 reqwest client

use hygiea::HyErr;
use hygiea::http_client::reqwest_client::{Client, ReqwestConfig};

/// 默认配置建的 client，mock server 是本地 http 地址，不需要额外配置
pub fn http_client() -> Result<Client, HyErr> {
    ReqwestConfig::default().build()
}
