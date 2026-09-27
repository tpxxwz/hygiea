// #[hy_err(..)] 只接受 `crate = "path"`
use hygiea::hy_err;

#[derive(hy_err)]
#[hy_err(krate = "::hygiea")]
enum E {
    #[error(err_code = "00001", err_tpl = "x")]
    A,
}

fn main() {}
