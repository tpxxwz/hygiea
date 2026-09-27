// #[redact()] 里什么都没写，既不是 mask 也不是 skip
use hygiea::redact::redact;
use serde::Serialize;

#[redact]
#[derive(Serialize)]
struct Login {
    #[redact()]
    password: String,
}

fn main() {}
