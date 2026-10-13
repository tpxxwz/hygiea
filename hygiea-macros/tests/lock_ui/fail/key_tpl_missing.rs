// lock_key 上没写 tpl
use hygiea::lock::hy_lock_key;

#[derive(hy_lock_key)]
enum K {
    #[lock_key(code = "000001")]
    A,
}

fn main() {}
