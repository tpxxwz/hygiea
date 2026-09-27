//! `#[derive(hy_err)]` 和 `err!` 的编译期检查：写错的用法必须在编译期报错，正确的用法必须能编译运行。
//!
//! 报错文案的快照在 `tests/hy_err_ui/fail/*.stderr`，改了报错信息后用
//! `TRYBUILD=overwrite cargo test -p hygiea-macros --test hy_err_ui` 重新生成，再人工核对 diff。
//!
//! 跨模块、跨 crate 的错误码重复编译期查不到：Linux 上链接期报重复符号，macOS 的 ld64 只给警告，
//! 最终由 hygiea-core 启动时的查重拦住（报错后 exit(1)）。trybuild 覆盖不到，这里只测同模块的

#[test]
fn hy_err_derive_fail() {
    let t = trybuild::TestCases::new();
    // err! 的一部分编译期检查（比如变量个数对不对）是宏生成的 `const { assert!(..) }` 块，
    // 只有在 trybuild 走 `cargo build`（而不是 `cargo check`）时才会真正求值报错；
    // trybuild 只有在同一个 TestCases 里注册过 pass 用例时才会用 build，所以这里搭一个必过的
    // pass 用例，把 has_pass 撑起来，不然这些用例会在 check 模式下悄悄通过
    t.pass("tests/hy_err_ui/pass/usage.rs");
    t.compile_fail("tests/hy_err_ui/fail/*.rs");
}

#[test]
fn hy_err_derive_pass() {
    let t = trybuild::TestCases::new();
    t.pass("tests/hy_err_ui/pass/*.rs");
}
