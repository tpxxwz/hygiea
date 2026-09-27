// 属性宏写在 derive 下面：derive 已经先展开，打码会悄悄失效，所以必须报错
use hygiea::redact::redact;
use serde::Serialize;

#[derive(Serialize)]
#[redact]
struct Login {
    #[redact(mask)]
    password: String,
}

fn main() {}
