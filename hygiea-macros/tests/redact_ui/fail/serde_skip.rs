// 字段已经被 serde 跳过，序列化时根本不出现
use hygiea::redact::redact;
use serde::Serialize;

#[redact]
#[derive(Serialize)]
struct Login {
    #[serde(skip)]
    #[redact(mask)]
    password: String,
}

fn main() {}
