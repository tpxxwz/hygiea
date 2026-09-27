// 项目前缀从 Cargo.toml 的 metadata 读；这里 workspace 和 crate 都没配，所以是默认的 "000"。
// err_code_module_prefix 是可选的模块前缀，写了之后变体 err_code 是 3 位，总位数仍是 8
use hygiea::{err, hy_err};

#[derive(hy_err)]
pub enum NoModule {
    #[error(err_code = "00042", err_tpl = "x")]
    A,
}

#[derive(hy_err)]
#[err_code_module_prefix = "02"]
pub enum WithModule {
    #[error(err_code = "007", err_tpl = "y")]
    B,
}

fn main() {
    assert_eq!(err!(NoModule::A).err_code(), "00000042");
    assert_eq!(err!(WithModule::B).err_code(), "00002007");
}
