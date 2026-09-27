// #[hy_err(crate = ..)] 的值必须是字符串字面量
use hygiea::hy_err;

#[derive(hy_err)]
#[hy_err(crate = 123)]
enum E {
    #[error(err_code = "00001", err_tpl = "x")]
    A,
}

fn main() {}
