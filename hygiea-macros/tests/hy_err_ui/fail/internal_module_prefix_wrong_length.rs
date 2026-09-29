// err_code_internal_module_prefix 必须正好 3 位数字
use hygiea::hy_err;

#[derive(hy_err)]
#[err_code_internal_module_prefix = "10"]
enum E {
    #[error(err_code = "01", err_tpl = "x")]
    A,
}

fn main() {}
