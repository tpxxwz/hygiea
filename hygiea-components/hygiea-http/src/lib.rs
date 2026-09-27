//! hygiea 的 HTTP 服务组件。
//!
//! | feature | 组件 |
//! |---|---|
//! | `axum` | [`AxumComponent`] |
//!
//! 收到退出信号后优雅关闭：不再接新请求，等手上的请求处理完（最多 `shutdown_timeout_secs`）。
//!
//! ```toml
//! hygiea-http = { version = "..", features = ["axum"] }
//! ```

#[cfg(feature = "axum")]
mod axum_server;

#[cfg(feature = "axum")]
pub use axum_server::*;
