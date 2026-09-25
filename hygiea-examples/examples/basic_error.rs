//! Basic error handling example
//!
//! Run with:
//! ```bash
//! cargo run --example basic_error --features error
//! ```

use hygiea::{err, hy_err};

// 项目前缀配在 hygiea-examples/Cargo.toml 的 [package.metadata.hygiea] err_code_project_prefix = "001"，
// 这里只写模块前缀（2 位），变体 err_code 写 3 位：001 + 01 + 001 = 00101001

// Define template-based errors
#[derive(hy_err)]
#[err_code_module_prefix = "01"]
pub enum UserErrors {
    #[error(err_code = "001", err_tpl = "User {{ name }} not found")]
    UserNotFound,

    #[error(err_code = "002", err_tpl = "Invalid email: {{ email }}")]
    InvalidEmail,

    #[error(err_code = "003", err_tpl = "User {{ username }} already exists")]
    UserExists,
}

// Define fixed-message errors (templates without variables)
#[derive(hy_err)]
#[err_code_module_prefix = "02"]
pub enum OrderErrors {
    #[error(err_code = "001", err_tpl = "Database connection failed")]
    DbConnectionFailed,

    #[error(err_code = "002", err_tpl = "Configuration error")]
    ConfigError,

    #[error(err_code = "003", err_tpl = "Service unavailable")]
    ServiceUnavailable,
}

fn main() {
    println!("=== Hygiea Error Handling Examples ===\n");

    // Example 1: Formatted error with template
    println!("1. Formatted Error (Template-based):");
    let err = err!(UserErrors::UserNotFound, "Alice");
    println!("   Error Code: {}", err.err_code());
    println!("   Message: {}\n", err);

    // Example 2: Another formatted error
    println!("2. Invalid Email Error:");
    let err = err!(UserErrors::InvalidEmail, "invalid-email");
    println!("   Error Code: {}", err.err_code());
    println!("   Message: {}\n", err);

    // Example 3: fixed message, no template variables
    println!("3. Fixed Message Error:");
    let err = err!(OrderErrors::DbConnectionFailed);
    println!("   Error Code: {}", err.err_code());
    println!("   Message: {}\n", err);

    // Example 4: Using base errors
    println!("4. Base System Errors:");
    use hygiea::BaseErr;
    // 对外消息固定为 "System Error"，原因挂在 source 上，只有 {:#} 才打出来
    let err = err!(BaseErr::SysErr).with_source("network timeout");
    println!("   Error Code: {}", err.err_code());
    println!("   Message: {}", err);
    println!("   With source: {:#}\n", err);

    // Example 5: Error as std::error::Error
    println!("5. Using as standard Error trait:");
    let err = err!(OrderErrors::ServiceUnavailable);
    print_error(&err);
}

fn print_error(err: &dyn std::error::Error) {
    println!("   Standard Error: {}", err);
}
