// flatten 的字段必须序列化成 map，遮成字符串会失败
use std::collections::HashMap;

use hygiea::redact::redact;
use serde::Serialize;

#[redact]
#[derive(Serialize)]
struct Login {
    #[serde(flatten)]
    #[redact(mask)]
    extra: HashMap<String, String>,
}

fn main() {}
