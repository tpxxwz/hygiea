# Hygiea

> A comprehensive Rust toolkit with error handling, template rendering, application framework, and more

[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)

## Overview

`hygiea` provides a unified, well-tested foundation for error handling, template rendering, application lifecycle management, and more across Rust projects.

## Features

### Currently Available

- **Template Rendering** - Template-based string rendering with MiniJinja, with both one-shot and cached variants
- **Error Handling** - Template-based error messages, automatic error code management with compile-time validation, distributed slice registration, two error types: `FmtErr` (template-based) and `RawErr` (fixed message)
- **Application Framework** - Component lifecycle management, async startup hooks, graceful shutdown via broadcast signals

### Planned

- **String Utilities** - String manipulation and formatting helpers
- **HTTP Utilities** - HTTP client and server utilities
- **JSON Utilities** - JSON processing and manipulation
- **Time Utilities** - Date and time operations

## Quick Start

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
hygiea = { version = "0.0.1", features = ["error"] }
```

**Available Features:**
- `template` - Template rendering with MiniJinja
- `error` - Error handling with template-based messages
- `app` - Component-based application framework
- `string` - String utilities (planned)
- `http` - HTTP utilities (planned)
- `json` - JSON utilities (planned)
- `time` - Time utilities (planned)
- `full` - Enable all features

**Examples:**

```toml
# Only error handling
hygiea = { version = "0.0.1", features = ["error"] }

# Multiple features
hygiea = { version = "0.0.1", features = ["error", "template"] }

# All features
hygiea = { version = "0.0.1", features = ["full"] }
```

### Basic Usage

```rust
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
}

// Define fixed-message errors
#[derive(raw_err)]
#[err_code_prefix = "002"]
pub enum SystemErrors {
    #[error(err_code = "00001", err_msg = "Database connection failed")]
    DbConnectionFailed,
}

fn main() {
    // Use formatted error
    let err = UserErrors::UserNotFound.to_err(json!({
        "name": "Alice"
    }));
    println!("Error: {}", err);
    // Output: User Alice not found

    // Use raw error
    let err = SystemErrors::DbConnectionFailed.to_err();
    println!("Error: {}", err);
    // Output: Database connection failed
}
```

## Architecture

This library follows a facade pattern with three internal crates:

```
hygiea/                    (Public API - what users depend on)
├── hygiea-core/           (Core implementation)
└── hygiea-macros/         (Procedural macros)
```

Users only need to depend on `hygiea`, which re-exports everything needed.

## Error Code System

Error codes follow an **8-digit format**: `PPPNNNNN`

- **PPP**: Prefix (3 digits) - Module/domain identifier
- **NNNNN**: Number (5 digits) - Specific error identifier

### Example

```rust
#[derive(fmt_err)]
#[err_code_prefix = "001"]  // ← Prefix
pub enum UserErrors {
    #[error(err_code = "00001", err_tpl = "...")]  // ← Number
    //                └─────┘
    //                5 digits
    //  Final code: 00100001
    UserNotFound,
}
```

### Built-in Error Codes

- `99909999` - Base raw system error
- `99999999` - Base formatted system error

## Feature Flags

```toml
[dependencies]
hygiea = { version = "0.0.1", features = ["full"] }
```

Available features:

- `template` - Template rendering with MiniJinja
- `error` - Error handling functionality
- `app` - Component-based application framework
- `string` - String utilities (planned)
- `http` - HTTP utilities (planned)
- `json` - JSON utilities (planned)
- `time` - Time utilities (planned)
- `full` - Enable all features

## Examples

See the `hygiea-examples/examples/` directory for complete examples:

```bash
# Run examples
cargo run -p hygiea-examples --example basic_error
cargo run -p hygiea-examples --example template
cargo run -p hygiea-examples --example app_framework
```

## Development

### Project Structure

```
hygiea/
├── Cargo.toml                  (Workspace config)
├── README.md
├── LICENSE-MIT
├── LICENSE-APACHE
│
├── hygiea/                     (Main facade crate)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── error.rs
│       └── template.rs
│
├── hygiea-core/                (Core implementation)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── error.rs
│       ├── template.rs
│       └── app.rs
│
├── hygiea-macros/              (Procedural macros)
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
│
├── hygiea-examples/            (Usage examples)
│   └── examples/
│       ├── basic_error.rs
│       ├── template.rs
│       └── app_framework.rs
│
└── target/                     (Build artifacts)
```

### Building

```bash
# Build all crates
cargo build

# Build with all features
cargo build --all-features

# Run tests
cargo test

# Check documentation
cargo doc --open
```

## Design Principles

1. **Facade Pattern**: Users only interact with `hygiea`, internal structure is hidden
2. **Feature Gated**: Only compile what you need
3. **Zero Cost**: Abstractions compile away, no runtime overhead
4. **Type Safe**: Leverage Rust's type system for correctness
5. **Ergonomic**: Easy to use, hard to misuse

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contact

Author: wjj (tpxxwz)
Email: tpxxwz@gmail.com
GitHub: [@tpxxwz](https://github.com/tpxxwz)

## Acknowledgments

This project is inspired by:

- [serde](https://github.com/serde-rs/serde) - Facade pattern and workspace organization
- [thiserror](https://github.com/dtolnay/thiserror) - Proc-macro architecture
- [anyhow](https://github.com/dtolnay/anyhow) - Error handling ergonomics
