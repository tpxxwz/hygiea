//! # WJJ Standard Library
//!
//! WJJ's comprehensive standard library for Rust projects, providing a unified
//! toolkit with error handling, utilities, and more.
//!
//! ## Features
//!
//! - **Error Handling**: Template-based error generation with automatic error code management
//! - **String Utils**: String manipulation utilities
//! - **HTTP Utils**: HTTP client utilities
//! - **JSON Utils**: JSON processing utilities (coming soon)
//! - **Time Utils**: Time and date utilities (coming soon)
//!
//! ## Quick Start
//!
//! ### Error Handling
//!
//! ```rust
//! # #[cfg(feature = "error")]
//! # {
//! use hygiea::{err, hy_err};
//!
//! // Templates with variables use err!(X, { .. }); fixed messages use err!(X).
//! // Using the wrong form is a compile error.
//! // The project prefix comes from Cargo.toml ([package.metadata.hygiea] err_code_project_prefix);
//! // each enum picks a 2-digit module prefix, and variants use 3-digit codes.
//! #[derive(hy_err)]
//! #[err_code_module_prefix = "01"]
//! pub enum UserErrors {
//!     #[error(err_code = "001", err_tpl = "User {{ name }} not found")]
//!     UserNotFound,
//!
//!     #[error(err_code = "002", err_tpl = "Invalid email: {{ email }}")]
//!     InvalidEmail,
//! }
//!
//! #[derive(hy_err)]
//! #[err_code_module_prefix = "02"]
//! pub enum SystemErrors {
//!     #[error(err_code = "001", err_tpl = "Database connection failed")]
//!     DbConnectionFailed,
//!
//!     #[error(err_code = "002", err_tpl = "Configuration error")]
//!     ConfigError,
//! }
//!
//! fn main() {
//!     // Template with variables
//!     let err = err!(UserErrors::UserNotFound, "Alice");
//!     println!("Error: {}", err);  // Error: User Alice not found
//!
//!     // Fixed message
//!     let err = err!(SystemErrors::DbConnectionFailed);
//!     println!("Error: {}", err);  // Error: Database connection failed
//! }
//! # }
//! ```
//!
//! ## Architecture
//!
//! This library uses a facade pattern with three internal crates:
//! - `hygiea`: Public API (this crate)
//! - `hygiea-core`: Core implementation
//! - `hygiea-macros`: Procedural macros
//!
//! ## Feature Flags
//!
//! Always available (no feature needed): error handling, `redact`, `datetime`, `env`, `string`.
//!
//! - `http`: HTTP client on top of reqwest
//! - `log`: tracing setup (implies `datetime-iana`)
//! - `app`: Component-based application framework (implies `log`)
//! - `distributed-lock`: `DistributedLock` trait
//! - `datetime-iana` / `datetime-chrono`: IANA timezones, chrono bridge
//! - `ws`: WebSocket client (work in progress, currently empty)
//! - `json`: Reserved, currently empty
//! - `full`: Enable all features
//!
//! ## Error Code System
//!
//! Error codes follow an 8-digit format: `PPPNNNNN`
//! - `PPP`: Prefix (3 digits) - Module identifier
//! - `NNNNN`: Number (5 digits) - Specific error identifier
//!
//! Example: `00100001` = Module `001`, Error `00001`

#![doc(html_root_url = "https://docs.rs/hygiea/0.0.1")]
#![deny(missing_docs)]

// 为了让宏生成的代码能找到 ::hygiea:: 路径
extern crate self as hygiea;

// ========== 常开 ==========
#[doc(hidden)]
pub use hygiea_core::__private;
pub use hygiea_core::{
    BaseErr, ErrKind, HyErr, ResultExt, SUCCESS_CODE, bail, datetime, env, err, hy_err, redact,
    sync,
};

// ========== 按 feature，模块名与 hygiea-core 一致 ==========
#[cfg(feature = "log")]
pub use hygiea_core::log;

#[cfg(feature = "app")]
pub use hygiea_core::app;

#[cfg(any(feature = "http", feature = "ws"))]
pub use hygiea_core::net;

/// 字符串工具：core 的正则和模板函数，加上 `fmt_tpl!` 等宏（宏由 `#[macro_export]` 导出在 crate 根）
pub mod string;

// ========== Feature: json ==========
/// JSON utilities module (coming soon)
#[cfg(feature = "json")]
pub mod json {}
