//! 直接依赖 hygiea-core（不经过 facade）时 `#[derive(hy_err)]` 的展开：不写 `crate = ...`，
//! 宏按调用方的依赖名自动生成 `::hygiea_core::...` 路径。
//!
//! 项目前缀取本 crate 的 `[package.metadata.hygiea]`（999），内部模块前缀用 core 测试专用的 `090`～`099`
//! （见 README「保留和内置错误码」）。
//!
//! 错误码靠 linkme 的 `distributed_slice` 在链接期收集，唯一性只在同一个可执行文件里校验。
//! `tests/` 下每个文件编译成单独的测试二进制，这个 enum 只和链接进来的 hygiea-core 自己的错误一起校验，
//! 不会进任何正式程序；用测试专用的区间，正式模块怎么加都不会和它撞

use hygiea_core::{err, hy_err};

#[derive(hy_err)]
#[err_code_internal_module_prefix = "090"]
enum InternalErr {
    #[error(err_code = "01", err_tpl = "Order {{ id }} not found")]
    OrderNotFound,
    #[error(err_code = "02", err_tpl = "Fixed message")]
    Fixed,
}

#[test]
fn derive_without_crate_arg_resolves_hygiea_core_path() {
    let e = err!(InternalErr::OrderNotFound, 42);
    assert_eq!(e.err_code(), "99909001");
    assert_eq!(e.to_string(), "Order 42 not found");
    assert!(e.is(InternalErr::OrderNotFound));
    assert!(!e.is(InternalErr::Fixed));
}

#[test]
fn fixed_message_and_source_chain() {
    let io = std::io::Error::other("disk full");
    let e = err!(InternalErr::Fixed).with_source(io);
    assert_eq!(e.err_code(), "99909002");
    assert_eq!(e.to_string(), "Fixed message");
    assert_eq!(format!("{e:#}"), "Fixed message: disk full");
}
