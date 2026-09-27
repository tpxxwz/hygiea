// #[error] 里的值根本不是字面量（不是字符串、也不是别的字面量，而是一个标识符）
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code = FOO, err_tpl = "x")]
    A,
}

fn main() {}
