// serde(into) 序列化的是目标类型，这里的标记不起作用
use hygiea::redact::redact;
use serde::Serialize;

#[redact]
#[derive(Serialize, Clone)]
#[serde(into = "String")]
struct Login {
    #[redact(mask)]
    password: String,
}

impl From<Login> for String {
    fn from(l: Login) -> String {
        l.password
    }
}

fn main() {}
