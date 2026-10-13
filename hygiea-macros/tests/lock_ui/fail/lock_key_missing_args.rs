// 模板有变量却没传
use hygiea::lock::{hy_lock_key, lock_key};

#[derive(hy_lock_key)]
enum K {
    #[lock_key(code = "000001", tpl = "user:{{ id }}")]
    A,
}

fn main() {
    let _ = lock_key!(K::A);
}
