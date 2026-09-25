// #[hy_err(crate = "..")] 显式指定生成代码里引用 hygiea 的路径
use hygiea::{err, hy_err};

#[derive(hy_err)]
#[hy_err(crate = "::hygiea")]
#[err_code_module_prefix = "09"]
pub enum E {
    #[error(err_code = "001", err_tpl = "x")]
    A,
}

fn main() {
    assert_eq!(err!(E::A).err_code, "00009001");
}
