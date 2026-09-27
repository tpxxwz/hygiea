// err_code 必须正好 5 位
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = "0001", err_tpl = "x")]
    A,
}

fn main() {}
