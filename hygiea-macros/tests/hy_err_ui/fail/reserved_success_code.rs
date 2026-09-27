// 00000000 留给成功响应，不能当错误码
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = "00000", err_tpl = "x")]
    A,
}

fn main() {}
