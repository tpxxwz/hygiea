// 每个 variant 必须有 err_code
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_tpl = "x")]
    A,
}

fn main() {}
