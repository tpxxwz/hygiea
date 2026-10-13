// 同一个模块里的两个 enum 拼出了相同的值：靠宏生成的常量 `HYGIEA_LOCK_TYPE_<9 位值>` 重名，rustc 报 E0428
use hygiea::lock::hy_lock_type;

#[derive(hy_lock_type)]
enum E1 {
    #[lock_type(code = "000001")]
    A,
}

#[derive(hy_lock_type)]
enum E2 {
    #[lock_type(code = "000001")]
    B,
}

fn main() {}
