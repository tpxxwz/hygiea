// 元组 struct 没有字段名，生成不了字段方法
use hygiea::db::live_config::hy_live_config;

#[derive(hy_live_config)]
struct Pair(u32, u32);

fn main() {}
