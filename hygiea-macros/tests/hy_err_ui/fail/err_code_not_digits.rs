// err_code 只能是数字
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = "0000a", err_tpl = "x")]
    A,
}

fn main() {}
