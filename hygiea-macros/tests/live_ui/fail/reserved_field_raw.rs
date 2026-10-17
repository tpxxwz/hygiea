// 原始标识符也算重名：on_change_by
use hygiea::db::live_config::Live;

#[derive(Live)]
struct Job {
    r#on_change_by: bool,
}

fn main() {}
