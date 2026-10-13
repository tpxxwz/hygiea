//! `#[derive(hy_lock_type)]`、`#[derive(hy_lock_key)]`、`lock_key!` 的编译期检查：写错的用法必须在编译期报错，正确的用法必须能编译运行。
//!
//! 报错文案的快照在 `tests/lock_ui/fail/*.stderr`，改了报错信息后用
//! `TRYBUILD=overwrite cargo test -p hygiea-macros --test lock_ui` 重新生成，再人工核对 diff。
//!
//! 跨模块、跨 crate 的重复编译期查不到：Linux 上链接期报重复符号，macOS 的 ld64 只给警告，
//! 最终由 hygiea-core 启动时的查重拦住（报错后 exit(1)）。trybuild 覆盖不到，这里只测同模块的

#[test]
fn lock_derive_fail() {
    let t = trybuild::TestCases::new();
    // lock_key! 的一部分检查是 const 块，trybuild 要有 pass 用例才走 cargo build 真正求值，见 hy_err_ui.rs
    t.pass("tests/lock_ui/pass/usage.rs");
    t.compile_fail("tests/lock_ui/fail/*.rs");
}

#[test]
fn lock_derive_pass() {
    let t = trybuild::TestCases::new();
    t.pass("tests/lock_ui/pass/*.rs");
}
