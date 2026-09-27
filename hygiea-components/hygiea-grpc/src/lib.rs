//! hygiea 的 gRPC 服务组件。
//!
//! | feature | 组件 |
//! |---|---|
//! | `tonic` | [`TonicComponent`]，路由用 [`tonic_serve_fn!`] 包装 |
//!
//! 收到退出信号后优雅关闭：不再接新请求，等手上的请求处理完（最多 `shutdown_timeout_secs`）。
//!
//! ```toml
//! hygiea-grpc = { version = "..", features = ["tonic"] }
//! ```

#[cfg(feature = "tonic")]
mod tonic_server;

#[cfg(feature = "tonic")]
pub use tonic_server::*;
