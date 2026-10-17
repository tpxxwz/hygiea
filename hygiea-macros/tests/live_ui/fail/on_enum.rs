// 只支持具名字段的 struct
use hygiea::db::live_config::hy_live_config;

#[derive(hy_live_config)]
enum Mode {
    Fast,
    Slow,
}

fn main() {}
