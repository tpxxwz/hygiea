// 模板语法错误在编译期报在 tpl 上
use hygiea::lock::hy_lock_key;

#[derive(hy_lock_key)]
enum K {
    #[lock_key(code = "000001", tpl = "user:{{ id")]
    A,
}

fn main() {}
