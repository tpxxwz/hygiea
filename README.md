# Hygiea

> A comprehensive Rust toolkit with error handling, log redaction, HTTP client, datetime utilities, application framework, and more

[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

## Overview

`hygiea` provides a unified, well-tested foundation for error handling, logging, HTTP calls, datetime handling, application lifecycle management, and more across Rust projects. Users depend on the single facade crate `hygiea` and enable only the features they need.

## Features

### Always available

These need no feature flag.

| Module | What it provides |
|---|---|
| Error handling (`hygiea::{HyErr, err!, bail!, hy_err}`) | Template-based error messages, 8-digit error codes validated at compile time, a single `HyErr` type, external errors attached via `with_source` |
| Redaction (`hygiea::redact`) | `#[redact]` attribute: fields marked `#[redact(mask)]` / `#[redact(skip)]` are masked only when serialized for logs (at any nesting depth); normal serialization is unaffected |
| Datetime (`hygiea::datetime`) | UTC datetime helpers on top of the `time` crate: formatting, parsing, start/end of day/week/month/year |
| Env (`hygiea::env`) | Environment variable helpers |
| String (`hygiea::string`) | Cached regex helpers and template rendering (`fmt_tpl!`, `fmt_tpl_once!`) |
| Sync (`hygiea::sync`) | `TokenBucket` rate limiter |

### Optional features

| Feature | What it enables |
|---|---|
| `http` | HTTP client on top of reqwest: `ClientConfig`, `RequestConfig`, request/response logging with redaction |
| `log` | tracing setup: console and rolling file layers, filters (implies `datetime-iana`) |
| `app` | Component-based application framework: startup hooks, shared resources, graceful shutdown (implies `log`) |
| `distributed-lock` | `DistributedLock` trait and `DistributedKey` |
| `datetime-iana` | IANA timezone and system-local datetime (`*_local` methods) |
| `datetime-chrono` | Conversion bridge to and from chrono |
| `ws` | WebSocket client (work in progress, currently empty) |
| `json` | Reserved, currently empty |
| `full` | All of the above |

### Components

Optional application components live in separate crates under `hygiea-components/`: `http-axum`, `grpc-tonic`, `db-pg-seaorm`, `db-pg-sqlx`, `db-sqlite-sqlx`, `redis-fred`.

## Quick Start

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
# Error handling, redaction, datetime, env and string need no features
hygiea = "0.1.1-alpha.5"

# Enable what you need
hygiea = { version = "0.1.1-alpha.5", features = ["http"] }

# Everything
hygiea = { version = "0.1.1-alpha.5", features = ["full"] }
```

### Error handling

```rust
use hygiea::{err, hy_err};

// Project prefix "001" comes from Cargo.toml, see "Error Code System" below
#[derive(hy_err)]
#[err_code_module_prefix = "01"]
pub enum UserErrors {
    #[error(err_code = "001", err_tpl = "User {{ name }} not found")]
    UserNotFound,

    #[error(err_code = "002", err_tpl = "Invalid email: {{ email }} ({{ reason }})")]
    InvalidEmail,

    // Fixed messages are just templates without variables
    #[error(err_code = "003", err_tpl = "Database connection failed")]
    DbConnectionFailed,
}

fn main() {
    // One variable: pass the value directly
    let e = err!(UserErrors::UserNotFound, "Alice");
    println!("{e} [{}]", e.err_code()); // User Alice not found [00101001]

    // Several variables: name each one
    let e = err!(UserErrors::InvalidEmail, { "email": "a@b", "reason": "no domain" });
    println!("{e}"); // Invalid email: a@b (no domain)

    // No variables: err!(X). Using the wrong form for a template is a compile error
    let e = err!(UserErrors::DbConnectionFailed)
        .with_source(std::io::Error::other("connection refused"));
    println!("{e}");   // Database connection failed
    println!("{e:#}"); // Database connection failed: connection refused
    assert!(e.is(UserErrors::DbConnectionFailed));
}
```

`Display` only renders the template, which is what clients should see. `{:#}` appends the `source` chain for server-side logs.

### HTTP with redacted logs

Requires the `http` feature. Every request logs `http call start` / `http call success` at INFO and failures at WARN (configurable). Fields marked `#[redact(mask)]` show up as `"***"` in those logs (params, request bodies, URLs, and response bodies decoded by `send`), while the real values are sent and received. Non-2xx and undecodable responses are logged as-is, since that is what you need to debug a failure.

```rust
use hygiea::HyErr;
use hygiea::net::http::{Client, ClientConfig, HttpResponse, Json, Method, RequestConfig};
use hygiea::redact::redact;
use serde::{Deserialize, Serialize};

// #[redact] must be placed above #[derive(Serialize)]
#[redact]
#[derive(Serialize)]
struct LoginReq {
    user: String,
    #[redact(mask)]
    password: String,
}

#[redact]
#[derive(Serialize, Deserialize)]
struct LoginResp {
    #[redact(mask)]
    token: String,
}

async fn login(client: &Client) -> Result<String, HyErr> {
    let req = LoginReq { user: "alice".into(), password: "p@ss".into() };
    let resp: HttpResponse<Json<LoginResp>> =
        RequestConfig::with_body(Method::POST, "https://api.example.com/login", Json(req))
            .send(client)
            .await?;
    Ok(resp.body.0.token)
}

// Build the client once and share it: it owns the connection pool
// let client = ClientConfig::default().build()?;
```

## Architecture

This library follows a facade pattern:

```
hygiea/                    (Public API - what users depend on)
├── hygiea-core/           (Core implementation)
└── hygiea-macros/         (Procedural macros: #[derive(hy_err)], #[redact])
```

Users only need to depend on `hygiea`, which re-exports everything needed, so no extra dependencies (such as `linkme` or `serde_json`) are required downstream. Code generated by the macros follows the name you depend on: `hygiea`, a renamed dependency, or `hygiea-core` alone all work. Use `#[hy_err(crate = "path")]` / `#[redact(crate = "path")]` to override.

## Error Code System

Error codes are always **8 digits**, in up to three levels:

| Level | Digits | Where | If omitted |
|---|---|---|---|
| Project prefix | 3 | `err_code_project_prefix` in Cargo.toml metadata | `000` |
| Module prefix | 2 | `#[err_code_module_prefix = ".."]` on the enum | no module level |
| Error number | 3 with a module prefix, 5 without | `err_code` on the variant | required |

The project prefix is looked up in the crate's `[package.metadata.hygiea]`, then in the workspace root's `[workspace.metadata.hygiea]`, and defaults to `000`. It cannot be set on an enum, so every enum in a project shares it.

```toml
# workspace root Cargo.toml (a crate can override it in [package.metadata.hygiea])
[workspace.metadata.hygiea]
err_code_project_prefix = "001"
```

```rust
#[derive(hy_err)]
#[err_code_module_prefix = "01"]  // ← module prefix
pub enum UserErrors {
    #[error(err_code = "001", err_tpl = "...")]  // ← error number, 3 digits
    //  Final code: 001 01 001 = 00101001
    UserNotFound,
}

#[derive(hy_err)]                 // no module prefix
pub enum LegacyErrors {
    #[error(err_code = "00001", err_tpl = "...")]  // ← 5 digits
    //  Final code: 001 00001 = 00100001
    Old,
}
```

### Reserved and built-in codes

- `00000000` is reserved for success (`SUCCESS_CODE`); using it is a compile error.
- Project prefix `999` is used by the framework's built-in errors. `BaseErr` (modules that need no feature) has no module prefix and uses 5-digit codes, with the catch-all `SysErr` at `99999`; `hygiea::net::http::BaseHttpErr` (the `http` feature) uses module prefix `01`. Duplicates inside one module are compile errors; duplicates across modules or crates are caught at startup (the process prints the code and exits with status 1).

| Code | Variant | Template |
|---|---|---|
| `99900001` | `BaseErr::DateError` | Date error: {{ cause }} |
| `99900002` | `BaseErr::RegexError` | Invalid regex: {{ pattern }} |
| `99900003` | `BaseErr::JsonError` | JSON error: {{ cause }} |
| `99900004` | `BaseErr::TemplateError` | Template error: {{ cause }} |
| `99900005` | `BaseErr::EnvError` | Environment variable not set: {{ name }} |
| `99901001` | `BaseHttpErr::ClientBuildFailed` | Http client build failed |
| `99901101` | `BaseHttpErr::InvalidUrl` | Invalid url: {{ url }} |
| `99901102` | `BaseHttpErr::InvalidParams` | Invalid query params: {{ method }} {{ url }} |
| `99901103` | `BaseHttpErr::InvalidHeader` | Invalid header: {{ cause }} |
| `99901104` | `BaseHttpErr::RequestBuildFailed` | Http request build failed: {{ method }} {{ url }} |
| `99901201` | `BaseHttpErr::RequestFailed` | Http request failed: {{ method }} {{ url }} |
| `99901202` | `BaseHttpErr::NonSuccessStatus` | Http {{ status }}: {{ method }} {{ url }} |
| `99901203` | `BaseHttpErr::WriteFailed` | Write response body failed |
| `99999999` | `BaseErr::SysErr` | System Error |

## Examples

See the `hygiea-examples/examples/` directory for complete examples:

```bash
cargo run -p hygiea-examples --example basic_error
cargo run -p hygiea-examples --example template
cargo run -p hygiea-examples --example app_framework

# Parallel backtests with tokio virtual time (see docs/notes/virtual-time-and-parallelism.md)
cargo run -p hygiea-core --example backtest_parallel --release
```

## Development

### Project Structure

```
hygiea/
├── Cargo.toml                  (Workspace config)
├── README.md
├── docs/                       (Design notes, reviews and research, see docs/README.md)
├── release/, release.sh        (cargo-release config and script)
│
├── hygiea/                     (Facade crate: the only one users depend on)
│   ├── src/
│   │   ├── lib.rs              (Re-exports from hygiea-core, feature gates)
│   │   └── string.rs           (fmt_tpl! macros)
│   └── tests/                  (trybuild UI tests for #[derive(hy_err)] / err! / #[redact])
│
├── hygiea-core/                (Core implementation)
│   ├── src/
│   │   ├── error.rs            (HyErr, err! / bail!, BaseErr)
│   │   ├── redact.rs           (Field masking for logs: #[redact], to_redacted_json)
│   │   ├── datetime/           (UTC / IANA datetime helpers)
│   │   ├── env.rs              (Environment variable helpers)
│   │   ├── string/             (Regex and template utilities)
│   │   ├── sync/               (Token bucket; distributed lock trait [feature: distributed-lock])
│   │   ├── net/                (http: reqwest wrapper; ws: WebSocket, WIP) [feature: http / ws]
│   │   ├── log.rs              (tracing setup)                  [feature: log]
│   │   └── app.rs              (Component-based app framework)  [feature: app]
│   ├── tests/http/             (End-to-end tests for net::http)
│   └── examples/               (backtest_parallel: virtual time + parallel backtests)
│
├── hygiea-macros/              (Procedural macros: hy_err derive, redact attribute)
├── hygiea-test-support/        (Shared test helpers, dev-dependency only, not published)
├── hygiea-components/          (Optional app components: http-axum, grpc-tonic,
│                                db-pg-seaorm, db-pg-sqlx, db-sqlite-sqlx, redis-fred)
└── hygiea-examples/            (Usage examples, not published)
```

### Building

```bash
# Build all crates
cargo build

# Build with all features
cargo build --all-features

# Run all tests, including the compile-fail (trybuild) tests in hygiea/tests
cargo test --workspace --all-features

# After changing a macro error message, regenerate the trybuild snapshots and review the diff
TRYBUILD=overwrite cargo test -p hygiea --test hy_err_ui --test redact_ui

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

Licensed under either of Apache License, Version 2.0 or MIT License, at your option.

## Contact

Author: wjj (tpxxwz)
Email: tpxxwz@gmail.com
GitHub: [@tpxxwz](https://github.com/tpxxwz)

## Acknowledgments

This project is inspired by:

- [serde](https://github.com/serde-rs/serde) - Facade pattern and workspace organization
- [thiserror](https://github.com/dtolnay/thiserror) - Proc-macro architecture
