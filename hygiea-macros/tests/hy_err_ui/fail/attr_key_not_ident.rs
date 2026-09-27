// #[error] 里的 key 必须是单个标识符
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(a::err_code = "00001", err_tpl = "x")]
    A,
}

fn main() {}
