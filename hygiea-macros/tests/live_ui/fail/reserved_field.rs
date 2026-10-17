// 字段名和生成的方法 snapshot 重名
use hygiea::db::live_config::hy_live_config;

#[derive(hy_live_config)]
struct Job {
    snapshot: bool,
}

fn main() {}
