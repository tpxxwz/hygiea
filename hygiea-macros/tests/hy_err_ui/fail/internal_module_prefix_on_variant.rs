// err_code_internal_module_prefix 只能写在 enum 上
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[err_code_internal_module_prefix = "100"]
    #[error(err_code = "00001", err_tpl = "x")]
    A,
}

fn main() {}
