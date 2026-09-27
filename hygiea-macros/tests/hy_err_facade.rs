//! 经 `hygiea::` 路径派生 `hy_err`，走真实的 facade crate（不是 trybuild 编译期检查）。
//! 断言 `err_code`、`Display` 的渲染结果，以及带 source 时 `{:#}` 的拼接。

use hygiea::{HyErr, err, hy_err};
use std::io;

// hygiea-macros 自己的 Cargo.toml 没有 [package.metadata.hygiea]，项目前缀是默认的 "000"；
// 没写 err_code_module_prefix，变体的 err_code 是 5 位，最终码 = "000" + 变体码
#[derive(hy_err)]
enum DemoErr {
    #[error(err_code = "00001", err_tpl = "fixed message")]
    Fixed,
    #[error(err_code = "00002", err_tpl = "user {{ name }} not found")]
    NotFound,
}

#[test]
fn err_code_uses_default_project_prefix() {
    let e = err!(DemoErr::Fixed);
    assert_eq!(e.err_code(), "00000001");
}

#[test]
fn display_renders_template_with_arg() {
    let e = err!(DemoErr::NotFound, "alice");
    assert_eq!(e.to_string(), "user alice not found");
}

#[test]
fn alternate_display_appends_source_chain() {
    let source = io::Error::other("disk full");
    let e: HyErr = err!(DemoErr::Fixed).with_source(source);

    // 普通 {} 不带 source
    assert_eq!(e.to_string(), "fixed message");
    // {:#} 把 source 链拼在后面
    assert_eq!(format!("{e:#}"), "fixed message: disk full");
}
