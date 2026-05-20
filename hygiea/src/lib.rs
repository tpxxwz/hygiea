//! # WJJ Standard Library
//!
//! WJJ's comprehensive standard library for Rust projects, providing a unified
//! toolkit with error handling, utilities, and more.
//!
//! ## Features
//!
//! - **Error Handling**: Template-based error generation with automatic error code management
//! - **String Utils**: String manipulation utilities (coming soon)
//! - **HTTP Utils**: HTTP client utilities (coming soon)
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
//! use hygiea::{fmt_err, raw_err, FmtErr, RawErr};
//! use serde_json::json;
//!
//! // Template-based formatted error
//! #[derive(fmt_err)]
//! #[err_code_prefix = "001"]
//! pub enum UserErrors {
//!     #[error(err_code = "00001", err_tpl = "User {{ name }} not found")]
//!     UserNotFound,
//!
//!     #[error(err_code = "00002", err_tpl = "Invalid email: {{ email }}")]
//!     InvalidEmail,
//! }
//!
//! // Fixed message error
//! #[derive(raw_err)]
//! #[err_code_prefix = "002"]
//! pub enum SystemErrors {
//!     #[error(err_code = "00001", err_msg = "Database connection failed")]
//!     DbConnectionFailed,
//!
//!     #[error(err_code = "00002", err_msg = "Configuration error")]
//!     ConfigError,
//! }
//!
//! fn main() {
//!     // Use formatted error
//!     let err = UserErrors::UserNotFound.to_err(json!({
//!         "name": "Alice"
//!     }));
//!     println!("Error: {}", err);  // Error: User Alice not found
//!
//!     // Use raw error
//!     let err = SystemErrors::DbConnectionFailed.to_err();
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
//! - `error`: Error handling functionality
//! - `app`: Component-based application framework
//! - `string`: String utilities (coming soon)
//! - `http`: HTTP utilities (coming soon)
//! - `json`: JSON utilities (coming soon)
//! - `time`: Time utilities (coming soon)
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

// ========== Feature: format ==========
#[cfg(feature = "format")]
mod format;

#[cfg(feature = "format")]
pub use format::{tpl_cached, tpl_once, tpl_pos};

// ========== error ==========
#[doc(hidden)]
pub use hygiea_core::__private;
pub use hygiea_core::{BaseFmtErr, BaseRawErr, FmtErr, RawErr, SuccessRawErr, fmt_err, raw_err};

// ========== Feature: app ==========
#[cfg(feature = "app")]
pub use hygiea_core::app::{
    Component, IntoRegistryConfig, LaunchError, Registry, RegistryConfig, Resources, async_trait,
};
#[cfg(feature = "app")]
pub use hygiea_core::log::{ConsoleLayer, FileLayer, TracingConfig};

// ========== util ==========
pub use hygiea_core::util;
pub use hygiea_core::{BuiltinKey, EnvKey, env_get, env_get_opt, env_get_or, env_get_or_else};

// ========== date ==========
pub use hygiea_core::{DateFormat, DateTimeUtcExt, Formatter, OffsetDateTime, OffsetDateTimeExt, now_utc, set_now_utc};
pub use hygiea_core::{SimClock, TokenBucket};

// ========== Feature: http ==========
/// HTTP utilities module (coming soon)
#[cfg(feature = "http")]
pub mod http {}

// ========== Feature: json ==========
/// JSON utilities module (coming soon)
#[cfg(feature = "json")]
pub mod json {}
