// nested / by_ref 是字段上的，标在 struct 上报错
use hygiea::db::live_config::Live;

#[derive(Live)]
#[live(nested)]
struct Job {
    times: u32,
}

fn main() {}
