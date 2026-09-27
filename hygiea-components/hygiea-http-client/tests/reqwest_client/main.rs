//! http 模块的端到端测试：起本地服务，只用公开 API 发请求。
//!
//! - `client_config`：ReqwestConfig 各选项在真实请求上的效果
//! - `config_file`：从 TOML 反序列化 ReqwestConfig
//! - `send`：请求内容、响应返回方式、各类失败报什么错
//! - `logs`：日志什么时候打、打在什么级别
//! - `masking`：日志里的字段打码
//! - `retry`：带重试的发送
//! - `api_envelope`：`{code, msg, data}` 外层结构的示范写法
//!
//! ## 为什么有这个 main.rs
//!
//! Cargo 把 `tests/` 下每个顶层 `.rs` 文件各编成一个独立的测试二进制（各自是一个 crate），
//! 但不会去看子目录里的文件。要把一个子目录编成**一个**测试二进制，就得有个入口文件当 crate 根，
//! 再用 `mod xxx;` 把同目录的文件挂成它的模块——这个入口就是本文件。
//! `tests/<目录>/main.rs` 是 Cargo 默认认的入口，`Cargo.toml` 里的 `[[test]]` 又显式写了一遍
//! （名字 `reqwest_client`，所以用 `cargo test --test reqwest_client` 单独跑）。
//!
//! 不平铺在 `tests/` 下的原因：平铺的话每个文件都是一个单独的二进制，`support.rs` 也会被当成测试编译，
//! 每个文件还要各自 `mod support;`；几个二进制分别编译、链接也更慢。放进子目录后它们是同一个 crate 的模块，
//! 互相用 `crate::support::*` 引用，只链接一次。

// `extern crate` 只能写在 crate 根上，这里就是根。
// redact 属性宏生成的代码按 ::hygiea:: 路径引用，这个组件 crate 的测试里没有 hygiea（facade）依赖，
// 于是把 hygiea_core 以 hygiea 的名字引进来：之后 `::hygiea::xxx` 就解析到 hygiea_core
extern crate hygiea_core as hygiea;

// `mod xxx;` 会去找同目录的 xxx.rs（或 xxx/mod.rs），把它编成本 crate 的一个子模块
mod support;

mod api_envelope;
mod client_config;
mod config_file;
mod logs;
mod masking;
mod retry;
mod send;
