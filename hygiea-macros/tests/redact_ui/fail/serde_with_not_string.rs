// serde 的 with 值必须是字符串字面量（模块路径），不能直接写模块名
use hygiea::redact::redact;
use serde::Serialize;

mod upper {}

#[redact]
#[derive(Serialize)]
struct Login {
    #[redact(mask)]
    #[serde(with = upper)]
    password: String,
}

fn main() {}
