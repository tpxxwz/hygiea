// 每个 variant 必须有 err_tpl
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = "00001")]
    A,
}

fn main() {}
