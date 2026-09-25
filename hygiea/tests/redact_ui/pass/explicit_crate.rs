// #[redact(crate = "..")] 显式指定生成代码里引用 hygiea 的路径
use hygiea::redact::{redact, to_redacted_json};
use serde::Serialize;

#[redact(crate = "::hygiea")]
#[derive(Serialize)]
struct Login {
    user: &'static str,
    #[redact(mask)]
    password: &'static str,
}

fn main() {
    let log = to_redacted_json(&Login { user: "a", password: "p" }).unwrap();
    assert_eq!(log, r#"{"user":"a","password":"***"}"#);
}
