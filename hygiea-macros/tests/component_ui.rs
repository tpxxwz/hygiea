//! `#[component]` 属性宏的编译期检查：写错的用法报在用户写的那一行，Deferred 组件第一阶段读不到资源，
//! 正确的用法（Immediate / Deferred、带 `crate = ..`、带路径的 trait 名）能编译通过。
//!
//! 报错文案的快照在 `tests/component_ui/fail/*.stderr`，改了报错信息后用
//! `TRYBUILD=overwrite cargo test -p hygiea-macros --test component_ui` 重新生成，再人工核对 diff

#[test]
fn component_attribute_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/component_ui/fail/*.rs");
}

#[test]
fn component_attribute_pass() {
    let t = trybuild::TestCases::new();
    t.pass("tests/component_ui/pass/*.rs");
}
