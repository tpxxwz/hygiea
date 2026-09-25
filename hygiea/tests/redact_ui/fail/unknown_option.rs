// 只认 mask / skip
use hygiea::redact::redact;
use serde::Serialize;

#[redact]
#[derive(Serialize)]
struct Login {
    #[redact(hide)]
    password: String,
}

fn main() {}
