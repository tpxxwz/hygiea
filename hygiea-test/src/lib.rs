//! 给使用 hygiea 的项目写测试用的工具，作为 dev-dependency 引入，按 feature 选：
//!
//! | feature | 内容 |
//! |---|---|
//! | `log` | [`log::init_once`]：在测试里装一次 hygiea 的默认日志 |
//! | `http-mock` | [`http_mock`]：本地 mock HTTP 服务，TOML cassette + Rhai 脚本 + 接口约定校验 |
//! | `http-client` | [`http_client`]：测试用的 reqwest client |
//!
//! hygiea 自己各 crate 的测试工具在 workspace 里的 `test-support`，不发布，这里不包含它

#[cfg(feature = "log")]
pub mod log;

#[cfg(feature = "http-mock")]
pub mod http_mock;

#[cfg(feature = "http-client")]
mod client;
#[cfg(feature = "http-client")]
pub use client::http_client;
