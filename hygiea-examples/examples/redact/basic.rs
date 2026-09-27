//! 日志打码：`#[redact(mask)]` / `#[redact(skip)]`，以及 `to_redacted_json` 和普通 `serde_json`
//! 序列化的对比——打码只发生在 `to_redacted_json` 里，存库、回响应、发请求这些普通序列化不受影响。
//!
//! ```bash
//! cargo run -p hygiea-examples --example redact_basic
//! ```

use hygiea::redact::{redact, to_redacted_json};
use hygiea::{BaseErr, HyErr, ResultExt, err};
use serde::Serialize;

#[redact]
#[derive(Serialize)]
struct LoginRequest {
    username: String,
    // 打码显示成 "***"，看不出原值，也看不出长度
    #[redact(mask)]
    password: String,
    // 直接不出现在日志里
    #[redact(skip)]
    device_id: String,
}

fn main() -> Result<(), HyErr> {
    let req = LoginRequest {
        username: "alice".to_string(),
        password: "p@ssw0rd".to_string(),
        device_id: "device-123".to_string(),
    };

    println!("1. 普通序列化（存库、回响应、发请求都用这个，字段原样输出）:");
    let plain = serde_json::to_string(&req)
        .wrap_err(|| err!(BaseErr::JsonError, "serialize plain json failed"))?;
    println!("   {plain}\n");

    println!("2. 打码后的日志预览（只有经过 to_redacted_json 才会打码）:");
    println!("   {}\n", to_redacted_json(&req)?);

    // 嵌套在别的结构体、Vec 里的照样打码，不需要给外层类型额外标记
    #[derive(Serialize)]
    struct Batch {
        requests: Vec<LoginRequest>,
    }
    let batch = Batch {
        requests: vec![req],
    };
    println!("3. 打码跟着 serde 序列化进到每一层（Vec、嵌套结构体都一样）:");
    println!("   {}", to_redacted_json(&batch)?);

    Ok(())
}
