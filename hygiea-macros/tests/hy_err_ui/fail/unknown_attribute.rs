// #[error] 只认 err_code / err_tpl
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = "00001", err_tpl = "x", level = "warn")]
    A,
}

fn main() {}
