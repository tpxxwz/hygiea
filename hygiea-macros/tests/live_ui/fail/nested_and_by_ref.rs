// nested 和 by_ref 不能同时标
use hygiea::db::live_config::hy_live_config;

#[derive(Clone, hy_live_config)]
struct Retry {
    times: u32,
}

#[derive(hy_live_config)]
struct Job {
    #[live_config(nested, by_ref)]
    retry: Retry,
}

fn main() {}
