// 同一个 enum 里两个变体的 code 一样
use hygiea::lock::hy_lock_type;

#[derive(hy_lock_type)]
#[lock_module_prefix = "02"]
enum E {
    #[lock_type(code = "0001")]
    A,
    #[lock_type(code = "0001")]
    B,
}

fn main() {}
