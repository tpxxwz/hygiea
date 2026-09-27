// 项目前缀只能配在 Cargo.toml 的 [package / workspace.metadata.hygiea] err_code_project_prefix，
// 不能写在 enum 上
use hygiea::hy_err;

#[derive(hy_err)]
#[err_code_project_prefix = "123"]
enum E {
    #[error(err_code = "00001", err_tpl = "x")]
    A,
}

fn main() {}
