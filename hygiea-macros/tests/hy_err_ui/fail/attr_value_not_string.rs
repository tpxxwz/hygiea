// #[error] 里的值必须是字符串字面量
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = 1, err_tpl = "x")]
    A,
}

fn main() {}
