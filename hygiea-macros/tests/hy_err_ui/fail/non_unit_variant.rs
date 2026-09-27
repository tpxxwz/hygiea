// 只支持 unit variant
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = "00001", err_tpl = "x")]
    A(u8),
}

fn main() {}
