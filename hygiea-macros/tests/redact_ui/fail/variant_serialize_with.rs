// variant 整体由自己的 serialize_with 序列化，里面字段的标记不起作用
use hygiea::redact::redact;
use serde::{Serialize, Serializer};

fn as_zero<S: Serializer>(_: &String, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u8(0)
}

#[redact]
#[derive(Serialize)]
enum Event {
    #[serde(serialize_with = "as_zero")]
    Token(#[redact(mask)] String),
}

fn main() {}
