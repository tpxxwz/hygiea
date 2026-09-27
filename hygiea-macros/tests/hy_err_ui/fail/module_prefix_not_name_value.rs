// err_code_module_prefix 只能写成 `= "..."`
use hygiea::hy_err;

#[derive(hy_err)]
#[err_code_module_prefix("01")]
enum E {
    #[error(err_code = "001", err_tpl = "x")]
    A,
}

fn main() {}
