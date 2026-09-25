//! hygiea 各 crate 测试共用的工具，只作为 dev-dependency 使用，不发布。
//!
//! - 根上：[`Unserializable`] 这类通用小工具
//! - [`headers`]：构造请求头
//! - [`logs`]：捕获 tracing 输出，断言日志里实际打了什么
//! - [`http_server`]：本地 HTTP 服务，给 http 客户端的端到端测试用
//!
//! 单元测试（`src` 里的 `#[cfg(test)]`）和集成测试（`tests/`）都能用，不必各写一份

pub mod headers;
pub mod http_server;
pub mod logs;

use serde::{Serialize, Serializer};

/// 序列化必定失败的类型，用来走各处的序列化错误分支
pub struct Unserializable;

impl Serialize for Unserializable {
    fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("unserializable on purpose"))
    }
}
