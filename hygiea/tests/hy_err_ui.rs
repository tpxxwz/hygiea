//! `#[derive(hy_err)]` 和 `err!` 的编译期检查：写错的用法必须在编译期报错，正确的用法必须能编译运行。
//!
//! 报错文案的快照在 `tests/hy_err_ui/fail/*.stderr`，改了报错信息后用
//! `TRYBUILD=overwrite cargo test -p hygiea --test hy_err_ui` 重新生成，再人工核对 diff。
//!
//! 跨模块、跨 crate 的错误码重复编译期查不到：Linux 上链接期报重复符号，macOS 的 ld64 只给警告，
//! 最终由 hygiea-core 启动时的查重拦住（报错后 exit(1)）。trybuild 覆盖不到，这里只测同模块的

#[test]
fn hy_err_derive_and_err_macro() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/hy_err_ui/fail/*.rs");
    t.pass("tests/hy_err_ui/pass/*.rs");
}
