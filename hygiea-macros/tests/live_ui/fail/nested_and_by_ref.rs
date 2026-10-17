// nested 和 by_ref 不能同时标
use hygiea::db::live_config::Live;

#[derive(Clone, Live)]
struct Retry {
    times: u32,
}

#[derive(Live)]
struct Job {
    #[live(nested, by_ref)]
    retry: Retry,
}

fn main() {}
