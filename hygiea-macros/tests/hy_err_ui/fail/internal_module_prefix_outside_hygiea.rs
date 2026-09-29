// err_code_internal_module_prefix 只给 hygiea 自己的 crate（项目前缀 999）用
use hygiea::hy_err;

#[derive(hy_err)]
#[err_code_internal_module_prefix = "100"]
enum E {
    #[error(err_code = "01", err_tpl = "x")]
    A,
}

fn main() {}
