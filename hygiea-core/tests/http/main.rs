//! http 模块的端到端测试：起本地服务，只用公开 API 发请求。
//!
//! - `client_config`：ClientConfig 各选项在真实请求上的效果
//! - `send`：请求内容、响应返回方式、各类失败报什么错
//! - `logs`：日志什么时候打、打在什么级别
//! - `masking`：日志里的字段打码
//! - `api_envelope`：`{code, msg, data}` 外层结构的示范写法

#![cfg(feature = "http")]

// redact 属性宏生成的代码按 ::hygiea:: 路径引用，集成测试里没有 hygiea 这个 crate，起个别名
extern crate hygiea_core as hygiea;

mod support;

mod api_envelope;
mod client_config;
mod logs;
mod masking;
mod send;
