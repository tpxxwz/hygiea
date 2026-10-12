//! `db!` 的编译期检查：拿不到表名又没写 op、最后一步不是执行方法时编译报错，支持的写法都能编译通过。
//!
//! 报错文案的快照在 `tests/db_macro_ui/fail/*.stderr`，改了报错信息后用
//! `TRYBUILD=overwrite cargo test -p hygiea-db --all-features --test db_macro_ui` 重新生成，再人工核对 diff

#![cfg(all(feature = "seaorm", feature = "postgres"))]

#[test]
fn db_macro_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/db_macro_ui/fail/*.rs");
}

#[test]
fn db_macro_pass() {
    let t = trybuild::TestCases::new();
    t.pass("tests/db_macro_ui/pass/*.rs");
}
