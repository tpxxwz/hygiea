// 有 2 位模块前缀时 code 要 4 位
use hygiea::lock::hy_lock_type;

#[derive(hy_lock_type)]
#[lock_module_prefix = "02"]
enum E {
    #[lock_type(code = "001")]
    A,
}

fn main() {}
