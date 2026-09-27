//! hygiea：基于组件的 Rust 应用框架（`hygiea::app`），附带错误、日志、日期、字符串等基础功能的封装和扩展。
//!
//! 这是 facade crate，使用方只依赖它：core 的内容全部在根上重新导出，组件（db / redis / http / grpc）
//! 按 feature 导出到对应的子模块。用法、feature 列表、错误码规则见
//! [README](https://github.com/tpxxwz/hygiea#readme)。

#![deny(missing_docs)]

// core 的全部内容（包括经由 core 导出的过程宏、#[macro_export] 的宏）。哪些模块存在由 core 的 feature 决定
pub use hygiea_core::*;

// ========== 组件（各自一个 crate，core 不能依赖它们，所以由 facade 导出） ==========
/// 数据库组件：`db-sqlx-postgres`、`db-sqlx-sqlite`、`db-seaorm-postgres`
#[cfg(any(
    feature = "db-sqlx-postgres",
    feature = "db-sqlx-sqlite",
    feature = "db-seaorm-postgres"
))]
pub use hygiea_db as db;

/// Redis 组件：`redis-fred`
#[cfg(feature = "redis-fred")]
pub use hygiea_redis as redis;

/// HTTP 服务组件：`http-axum`。HTTP 客户端是 `hygiea::net::http_client`（`http-client` feature）
#[cfg(feature = "http-axum")]
pub use hygiea_http as http;

/// gRPC 服务组件：`grpc-tonic`
#[cfg(feature = "grpc-tonic")]
pub use hygiea_grpc as grpc;
