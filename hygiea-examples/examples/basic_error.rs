//! Basic error handling example
//!
//! Run with:
//! ```bash
//! cargo run --example basic_error --features error
//! ```

use hygiea::{fmt_err, raw_err};
use serde_json::json;

// Define template-based errors
#[derive(fmt_err)]
#[err_code_prefix = "001"]
pub enum UserErrors {
    #[error(err_code = "00001", err_tpl = "User {{ name }} not found")]
    UserNotFound,

    #[error(err_code = "00002", err_tpl = "Invalid email: {{ email }}")]
    InvalidEmail,

    #[error(err_code = "00003", err_tpl = "User {{ username }} already exists")]
    UserExists,
}

// Define fixed-message errors
#[derive(raw_err)]
#[err_code_prefix = "002"]
pub enum OrderErrors {
    #[error(err_code = "00001", err_msg = "Database connection failed")]
    DbConnectionFailed,

    #[error(err_code = "00002", err_msg = "Configuration error")]
    ConfigError,

    #[error(err_code = "00003", err_msg = "Service unavailable")]
    ServiceUnavailable,
}

fn main() {
    println!("=== Hygiea Error Handling Examples ===\n");

    // Example 1: Formatted error with template
    println!("1. Formatted Error (Template-based):");
    let err = UserErrors::UserNotFound.to_err(json!({
        "name": "Alice"
    }));
    println!("   Error Code: {}", err.err_code);
    println!("   Message: {}\n", err);

    // Example 2: Another formatted error
    println!("2. Invalid Email Error:");
    let err = UserErrors::InvalidEmail.to_err(json!({
        "email": "invalid-email"
    }));
    println!("   Error Code: {}", err.err_code);
    println!("   Message: {}\n", err);

    // Example 3: Raw error (fixed message)
    println!("3. Raw Error (Fixed Message):");
    let err = OrderErrors::DbConnectionFailed.to_err();
    println!("   Error Code: {}", err.err_code);
    println!("   Message: {}\n", err);

    // Example 4: Using base errors
    println!("4. Base System Errors:");
    use hygiea::BaseFmtErrs;
    let err = BaseFmtErrs::SysFmtErr.to_err(json!({
        "cause": "network timeout"
    }));
    println!("   Error Code: {}", err.err_code);
    println!("   Message: {}\n", err);

    // Example 5: Error as std::error::Error
    println!("5. Using as standard Error trait:");
    let err = OrderErrors::ServiceUnavailable.to_err();
    print_error(&err);
}

fn print_error(err: &dyn std::error::Error) {
    println!("   Standard Error: {}", err);
}
