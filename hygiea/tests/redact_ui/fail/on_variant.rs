// #[redact(..)] 不能写在 enum variant 上，要写在 variant 的字段上
use hygiea::redact::redact;
use serde::Serialize;

#[redact]
#[derive(Serialize)]
enum Event {
    #[redact(mask)]
    Token(String),
}

fn main() {}
