// #[redact(..)] 标记只能写在字段上
use hygiea::redact::redact;
use serde::Serialize;

#[redact]
#[redact(mask)]
#[derive(Serialize)]
struct Login {
    password: String,
}

fn main() {}
