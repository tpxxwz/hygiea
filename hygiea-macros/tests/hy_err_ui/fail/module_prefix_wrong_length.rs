// err_code_module_prefix 必须正好 2 位数字
use hygiea::hy_err;

#[derive(hy_err)]
#[err_code_module_prefix = "3"]
enum E {
    #[error(err_code = "001", err_tpl = "x")]
    A,
}

fn main() {}
