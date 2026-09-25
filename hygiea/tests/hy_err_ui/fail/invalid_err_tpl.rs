// err_tpl 语法错误在编译期报出来，不等到启动时
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = "00001", err_tpl = "user {{ name")]
    A,
}

fn main() {}
