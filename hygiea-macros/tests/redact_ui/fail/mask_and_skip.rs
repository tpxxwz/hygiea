// mask 和 skip 只能选一个
use hygiea::redact::redact;
use serde::Serialize;

#[redact]
#[derive(Serialize)]
struct Login {
    #[redact(mask, skip)]
    password: String,
}

fn main() {}
