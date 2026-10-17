//! `#[derive(Live)]` 的编译期检查：不支持的类型、和生成的方法重名的字段、写错的 `#[live(..)]` 在编译期报错，
//! 私有字段的方法在模块外调不到；正确的用法（三层嵌套、`crate = ..`）能编译通过。
//!
//! 报错文案的快照在 `tests/live_ui/fail/*.stderr`，改了报错信息后用
//! `TRYBUILD=overwrite cargo test -p hygiea-macros --test live_ui` 重新生成，再人工核对 diff

#[test]
fn live_derive_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/live_ui/fail/*.rs");
}

#[test]
fn live_derive_pass() {
    let t = trybuild::TestCases::new();
    t.pass("tests/live_ui/pass/*.rs");
}
