// err_code_module_prefix 和 err_code_internal_module_prefix 只能写一个
use hygiea::hy_err;

#[derive(hy_err)]
#[err_code_module_prefix = "01"]
#[err_code_internal_module_prefix = "100"]
enum E {
    #[error(err_code = "001", err_tpl = "x")]
    A,
}

fn main() {}
