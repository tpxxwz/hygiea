// err_code_module_prefix 的值必须是字符串字面量
use hygiea::hy_err;

#[derive(hy_err)]
#[err_code_module_prefix = 1]
enum E {
    #[error(err_code = "001", err_tpl = "x")]
    A,
}

fn main() {}
