//! hygiea-test 的过程宏，经由 `hygiea_test` 使用，不直接依赖。
//!
//! 宏只改写代码结构，运行时要做的事（比如检查环境变量）是 `hygiea_test::tier` 里的普通函数，
//! 生成的代码去调用它们。

use proc_macro::TokenStream;

mod test_tier;

/// 标在模块上，把里面的测试归到 container 层：需要容器运行时（Docker 等）。默认 `cargo test` 不跑，
/// 测试名是 `<模块名>::container::<函数名>`，用 `cargo test -- --ignored ::container::` 选中。见 `hygiea_test`
#[proc_macro_attribute]
pub fn container(attr: TokenStream, item: TokenStream) -> TokenStream {
    test_tier::expand(test_tier::Tier::Container, attr, item)
}

/// 标在模块上，把里面的测试归到 live 层：连使用者自己控制的真实服务。默认 `cargo test` 不跑，
/// 测试名是 `<模块名>::live::<函数名>`，用 `cargo test -- --ignored ::live::` 选中。
/// `#[live(env = ["A", "B"])]` 让每个测试运行前检查环境变量，缺了列出全部缺少的名字。见 `hygiea_test`
#[proc_macro_attribute]
pub fn live(attr: TokenStream, item: TokenStream) -> TokenStream {
    test_tier::expand(test_tier::Tier::Live, attr, item)
}
