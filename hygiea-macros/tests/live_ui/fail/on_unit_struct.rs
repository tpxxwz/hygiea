// 单元 struct 没有字段
use hygiea::db::live_config::hy_live_config;

#[derive(hy_live_config)]
struct Empty;

fn main() {}
