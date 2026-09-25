// 写了 err_code_module_prefix 时，变体的 err_code 是 3 位
use hygiea::hy_err;

#[derive(hy_err)]
#[err_code_module_prefix = "02"]
enum E {
    #[error(err_code = "00001", err_tpl = "x")]
    A,
}

fn main() {}
