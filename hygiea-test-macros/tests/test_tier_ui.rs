//! `#[container]` / `#[live]` 属性宏（标在模块上）的编译期检查：写错的用法报在用户写的那一行。
//! 正确用法的展开（测试名、ignore 原因、环境变量检查）由实际使用它们的测试覆盖，比如
//! `hygiea-aws/tests/s3.rs`。
//!
//! 报错文案的快照在 `tests/test_tier_ui/fail/*.stderr`，改了报错信息后用
//! `TRYBUILD=overwrite cargo test -p hygiea-macros --test test_tier_ui` 重新生成，再人工核对 diff

#[test]
fn test_tier_attribute_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/test_tier_ui/fail/*.rs");
}
