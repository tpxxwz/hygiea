// #[error(..)] 里每一项必须是 `key = "value"`，裸写一个标识符解析不了
use hygiea::hy_err;

#[derive(hy_err)]
enum E {
    #[error(err_code)]
    A,
}

fn main() {}
