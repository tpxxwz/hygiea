//! hygiea 的 HTTP 客户端组件：按实现分模块，模块里有请求 API、客户端配置和应用组件。
//!
//! | feature | 模块 | 组件 | 放进 Resources 的资源 |
//! |---|---|---|---|
//! | `reqwest` | [`reqwest_client`] | [`ReqwestComponent`](reqwest_client::ReqwestComponent) | `reqwest::Client` |
//!
//! 每种实现单独一个模块，不在 crate 根上再导出，以后加别的实现时名字不会撞：
//! `hygiea::http_client::reqwest_client::{ReqwestConfig, RequestConfig, …}`。
//!
//! ```toml
//! hygiea-http-client = { version = "..", features = ["reqwest"] }
//! ```

// 模块文档写在 reqwest_client/mod.rs 开头（`//!`）。这里不再加 `///`：外部和内部文档同时存在时，
// rustdoc 按父模块的作用域解析内部文档里的链接，mod.rs 里的 [`Client`] 之类就全都找不到了
#[cfg(feature = "reqwest")]
pub mod reqwest_client;
