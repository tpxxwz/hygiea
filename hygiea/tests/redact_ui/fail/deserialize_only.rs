// 只 derive 了 Deserialize：打码只作用于序列化，标了也没用
use hygiea::redact::redact;
use serde::Deserialize;

#[redact]
#[derive(Deserialize)]
struct Login {
    #[redact(mask)]
    password: String,
}

fn main() {}
