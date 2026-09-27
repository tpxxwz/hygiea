// #[redact(crate = ..)] 的值必须是字符串字面量
use hygiea::redact::redact;
use serde::Serialize;

#[redact(crate = 1)]
#[derive(Serialize)]
struct Login {
    password: String,
}

fn main() {}
