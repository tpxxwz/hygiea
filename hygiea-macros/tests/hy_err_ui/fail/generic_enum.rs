// 不支持泛型
use hygiea::hy_err;

#[derive(hy_err)]
enum E<T> {
    #[error(err_code = "00001", err_tpl = "x")]
    A,
    #[allow(dead_code)]
    #[error(err_code = "00002", err_tpl = "y")]
    B,
    _P(std::marker::PhantomData<T>),
}

fn main() {}
