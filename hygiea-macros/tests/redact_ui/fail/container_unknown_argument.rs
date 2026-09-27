// 类型上的 #[redact] 只接受 `crate = "path"`
use hygiea::redact::redact;
use serde::Serialize;

#[redact(mask)]
#[derive(Serialize)]
struct Login {
    password: String,
}

fn main() {}
