// err!(X, { .. }) 漏写或拼错了模板变量
use hygiea::{err, hy_err};

#[derive(hy_err)]
enum E {
    #[error(err_code = "00001", err_tpl = "{{ a }} and {{ b }}")]
    TwoVars,
}

fn main() {
    let _ = err!(E::TwoVars, { "a": 1, "bb": 2 });
}
