// 正确用法：只依赖 hygiea（不用自己加 linkme / serde_json），err! 三种写法、bail!、HyErr::is
use hygiea::{HyErr, bail, err, hy_err};

#[derive(hy_err)]
pub enum UserErr {
    #[error(err_code = "00001", err_tpl = "fixed message")]
    Fixed,
    #[error(err_code = "00002", err_tpl = "user {{ name }} not found")]
    NotFound,
    #[error(err_code = "00003", err_tpl = "{{ a }} and {{ b }}")]
    Pair,
}

fn check(n: i64) -> Result<(), HyErr> {
    if n < 0 {
        bail!(UserErr::NotFound, n);
    }
    Ok(())
}

fn main() {
    assert_eq!(err!(UserErr::Fixed).to_string(), "fixed message");
    assert_eq!(err!(UserErr::NotFound, "alice").to_string(), "user alice not found");
    assert_eq!(err!(UserErr::Pair, { "a": 1, "b": "x" }).to_string(), "1 and x");

    let e = err!(UserErr::NotFound, "bob");
    assert!(e.is(UserErr::NotFound));
    assert!(!e.is(UserErr::Fixed));
    assert_eq!(e.err_code(), "00000002");

    assert!(check(1).is_ok());
    assert_eq!(check(-1).unwrap_err().to_string(), "user -1 not found");
}
