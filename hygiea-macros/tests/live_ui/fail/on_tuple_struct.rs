// 元组 struct 没有字段名，生成不了字段方法
use hygiea::db::live_config::Live;

#[derive(Live)]
struct Pair(u32, u32);

fn main() {}
