// 单元 struct 没有字段
use hygiea::db::live_config::Live;

#[derive(Live)]
struct Empty;

fn main() {}
