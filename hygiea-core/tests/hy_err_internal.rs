//! 直接依赖 hygiea-core（不经过 facade）时 `#[derive(hy_err)]` 的展开：不写 `crate = ...`，
//! 宏按调用方的依赖名自动生成 `::hygiea_core::...` 路径。
//!
//! 项目前缀取本 crate 的 `[package.metadata.hygiea]`（999）；模块前缀用 98，避开 core 里已用的 01～03

use hygiea_core::{err, hy_err};

#[derive(hy_err)]
#[err_code_module_prefix = "98"]
enum InternalErr {
    #[error(err_code = "001", err_tpl = "Order {{ id }} not found")]
    OrderNotFound,
    #[error(err_code = "002", err_tpl = "Fixed message")]
    Fixed,
}

#[test]
fn derive_without_crate_arg_resolves_hygiea_core_path() {
    let e = err!(InternalErr::OrderNotFound, 42);
    assert_eq!(e.err_code(), "99998001");
    assert_eq!(e.to_string(), "Order 42 not found");
    assert!(e.is(InternalErr::OrderNotFound));
    assert!(!e.is(InternalErr::Fixed));
}

#[test]
fn fixed_message_and_source_chain() {
    let io = std::io::Error::other("disk full");
    let e = err!(InternalErr::Fixed).with_source(io);
    assert_eq!(e.err_code(), "99998002");
    assert_eq!(e.to_string(), "Fixed message");
    assert_eq!(format!("{e:#}"), "Fixed message: disk full");
}
