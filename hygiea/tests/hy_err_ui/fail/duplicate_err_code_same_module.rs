// 同一个模块里的两个 enum 拼出了相同的完整错误码
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
