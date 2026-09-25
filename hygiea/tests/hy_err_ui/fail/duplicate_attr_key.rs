// 同一个 #[error] 里重复写 key
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = "00001", err_tpl = "x", err_code = "00002")]
    A,
}

fn main() {}
