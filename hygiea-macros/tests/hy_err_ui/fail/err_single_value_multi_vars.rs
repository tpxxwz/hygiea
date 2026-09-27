// 多个变量不能用单值简写
use hygiea::{err, hy_err};

#[derive(hy_err)]
enum E {
    #[error(err_code = "00001", err_tpl = "fixed message")]
    Fixed,
    #[error(err_code = "00002", err_tpl = "user {{ name }} not found")]
    OneVar,
    #[error(err_code = "00003", err_tpl = "{{ a }} and {{ b }}")]
    TwoVars,
}

fn main() {
    let _ = err!(E::TwoVars, "v");
}
