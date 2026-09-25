// 同一个 enum 里错误码重复
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = "00001", err_tpl = "a")]
    A,
    #[error(err_code = "00001", err_tpl = "b")]
    B,
}

fn main() {}
