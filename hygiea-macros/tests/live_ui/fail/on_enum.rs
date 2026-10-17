// 只支持具名字段的 struct
use hygiea::db::live_config::Live;

#[derive(Live)]
enum Mode {
    Fast,
    Slow,
}

fn main() {}
