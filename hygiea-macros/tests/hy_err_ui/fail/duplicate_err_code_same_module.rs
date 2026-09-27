// 同一个模块里的两个 enum 拼出了相同的完整错误码。
// 每次 derive 各自展开，宏看不到别的 enum 用了哪些码，没法自己报错；靠的是宏给每个码生成的常量
// `HYGIEA_ERR_CODE_<完整错误码>` 在同一模块里重名，由 rustc 报 E0428（报错里带着重复的码）。
// 快照依赖这个常量名，改了命名规则要重新生成快照。
use hygiea::hy_err;

#[derive(hy_err)]
enum E1 {
    #[error(err_code = "00001", err_tpl = "a")]
    A,
}

#[derive(hy_err)]
enum E2 {
    #[error(err_code = "00001", err_tpl = "b")]
    B,
}

fn main() {}
