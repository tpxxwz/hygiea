// 一个字段只能有一个 #[redact(..)]
use hygiea::redact::redact;
use serde::Serialize;

#[redact]
#[derive(Serialize)]
struct Login {
    #[redact(mask)]
    #[redact(skip)]
    password: String,
}

fn main() {}
