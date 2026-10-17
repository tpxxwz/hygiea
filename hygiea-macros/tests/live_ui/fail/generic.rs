// 带泛型的 struct 不支持
use hygiea::db::live_config::Live;

#[derive(Live)]
struct Wrapper<T> {
    value: T,
}

fn main() {}
