// #[error] 只能写在 variant 上
use hygiea::hy_err;

#[derive(hy_err)]
#[error(err_code = "00001", err_tpl = "x")]
enum E {
    A,
}

fn main() {}
