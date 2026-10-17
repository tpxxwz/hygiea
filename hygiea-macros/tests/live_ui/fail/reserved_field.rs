// 字段名和生成的方法 snapshot 重名
use hygiea::db::live_config::Live;

#[derive(Live)]
struct Job {
    snapshot: bool,
}

fn main() {}
