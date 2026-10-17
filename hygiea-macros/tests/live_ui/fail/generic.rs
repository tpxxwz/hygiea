// 带泛型的 struct 不支持
use hygiea::db::live_config::hy_live_config;

#[derive(hy_live_config)]
struct Wrapper<T> {
    value: T,
}

fn main() {}
