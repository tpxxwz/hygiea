// 锁类型和字符串 key 共用编号：同一个模块里两个 derive 拼出相同的值，靠常量 `HYGIEA_LOCK_<9 位>` 重名报 E0428
use hygiea::lock::{hy_lock_key, hy_lock_type};

#[derive(hy_lock_type)]
enum T {
    #[lock_type(code = "000001")]
    A,
}

#[derive(hy_lock_key)]
enum K {
    #[lock_key(code = "000001", tpl = "x")]
    B,
}

fn main() {}
