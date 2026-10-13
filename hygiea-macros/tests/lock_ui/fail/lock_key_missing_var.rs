// { .. } 写法漏了模板变量
use hygiea::lock::{hy_lock_key, lock_key};

#[derive(hy_lock_key)]
enum K {
    #[lock_key(code = "000001", tpl = "{{ shop }}:{{ day }}")]
    A,
}

fn main() {
    let _ = lock_key!(K::A, { "shop": 1 });
}
