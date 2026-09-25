//! `#[redact]` 属性宏的编译期检查：写错的用法必须在编译期报错（打码不会悄悄失效），
//! 正确的用法必须能编译通过。
//!
//! 报错文案的快照在 `tests/redact_ui/fail/*.stderr`，改了报错信息后用
//! `TRYBUILD=overwrite cargo test -p hygiea --test redact_ui` 重新生成，再人工核对 diff

#[test]
fn redact_attribute() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/redact_ui/fail/*.rs");
    t.pass("tests/redact_ui/pass/*.rs");
}
