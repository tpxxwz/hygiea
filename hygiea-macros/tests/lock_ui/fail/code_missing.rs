// 变体上没写 code
use hygiea::lock::hy_lock_type;

#[derive(hy_lock_type)]
enum E {
    A,
}

fn main() {}
