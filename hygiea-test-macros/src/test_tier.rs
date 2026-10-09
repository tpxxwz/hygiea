use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;

// #[container] / #[live] 属性宏的实现：标在模块上，把模块里的测试归到对应的层，
// 默认 cargo test 不跑，按层过滤能选中。
//
// #[container]
// mod rustfs {
//     use super::*;
//
//     #[tokio::test]
//     async fn put_get() { .. }
//
//     fn helper() { .. }
// }
//
// 展开成：
//
// #[cfg(test)]
// mod rustfs {
//     #[allow(unused_imports)]
//     use super::*;
//
//     #[cfg(test)]
//     mod container {
//         use super::*;
//
//         #[tokio::test]
//         #[ignore = "container: needs a container runtime; run with `-- --ignored ::container::`"]
//         async fn put_get() { .. }
//
//         fn helper() { .. }
//     }
// }
//
// 测试名因此是 `rustfs::container::put_get`，`cargo test -- --ignored ::container::` 只选中这一层。
// 套一层层名模块是为了让层名出现在测试路径里，用户起的模块名照样保留，不同层的测试可以同名。
//
// 哪些是测试：带最后一段叫 test 的属性（`#[test]`、`#[tokio::test(..)]` 等）的函数，
// 宏给它们加 `#[ignore]`；其他项原样保留，辅助函数可以放在模块里。嵌套的内联模块也一样处理。
// #[live(env = ["A", "B"])] 还会在每个测试的函数体开头调用 `::hygiea_test::tier::require_env(&["A", "B"])`，
// 缺了就列出全部缺少的名字再 panic。逻辑在 hygiea-test 里，宏只生成调用。
//
// 模块内容被挪进了层名模块。外面那层 glob 引入了上一级，所以里面的 `super::xxx`、`use super::*` 照常能用；
// 只有往上跳两级以上的 `super::super::..` 要多写一个 `super::`。

/// 测试所在的层
#[derive(Clone, Copy)]
pub(crate) enum Tier {
    Container,
    Live,
}

impl Tier {
    fn name(self) -> &'static str {
        match self {
            Tier::Container => "container",
            Tier::Live => "live",
        }
    }
}

pub(crate) fn expand(tier: Tier, attr: TokenStream, item: TokenStream) -> TokenStream {
    match expand_inner(tier, attr.into(), item.into()) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_inner(
    tier: Tier,
    attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let env = parse_args(tier, attr)?;
    let module: syn::ItemMod = syn::parse2(item).map_err(|e| {
        syn::Error::new(
            e.span(),
            format!(
                "#[{}] goes on an inline module: `mod name {{ .. }}`",
                tier.name()
            ),
        )
    })?;
    let Some((_, items)) = module.content else {
        return Err(syn::Error::new_spanned(
            &module.ident,
            format!(
                "#[{}] needs an inline module `mod {} {{ .. }}`, not a module in another file",
                tier.name(),
                module.ident
            ),
        ));
    };

    let reason = ignore_reason(tier, &env);
    let env_check = env_check(&krate(), &env);
    let mut count = 0;
    let items = mark_tests(tier, items, &reason, env_check.as_ref(), &mut count)?;
    if count == 0 {
        return Err(syn::Error::new_spanned(
            &module.ident,
            format!(
                "#[{}] module has no test functions; mark them with #[test] / #[tokio::test]",
                tier.name()
            ),
        ));
    }

    // 内部属性（`#![..]`）跟着内容进层名模块，外部属性（文档注释等）留在外面
    let (inner_attrs, outer_attrs): (Vec<_>, Vec<_>) = module
        .attrs
        .into_iter()
        .partition(|a| matches!(a.style, syn::AttrStyle::Inner(_)));
    let vis = module.vis;
    let name = module.ident;
    let tier_mod = format_ident!("{}", tier.name(), span = name.span());

    // 这些层的模块只放测试，内外两层都固定只在测试编译时存在，不用调用方自己写 #[cfg(test)]
    Ok(quote! {
        #[cfg(test)]
        #(#outer_attrs)*
        #vis mod #name {
            #[allow(unused_imports)]
            use super::*;

            #[cfg(test)]
            mod #tier_mod {
                #(#inner_attrs)*
                #(#items)*
            }
        }
    })
}

/// 给带测试属性的函数加 `#[ignore]` 和环境变量检查，嵌套的内联模块递归处理。`count` 累计找到的测试数
fn mark_tests(
    tier: Tier,
    items: Vec<syn::Item>,
    reason: &str,
    env_check: Option<&syn::Stmt>,
    count: &mut usize,
) -> syn::Result<Vec<syn::Item>> {
    items
        .into_iter()
        .map(|item| match item {
            syn::Item::Fn(mut func) if is_test(&func) => {
                if let Some(ignore) = func.attrs.iter().find(|a| a.path().is_ident("ignore")) {
                    return Err(syn::Error::new_spanned(
                        ignore,
                        format!("#[{}] already adds #[ignore], remove this one", tier.name()),
                    ));
                }
                func.attrs.push(syn::parse_quote!(#[ignore = #reason]));
                if let Some(check) = env_check {
                    func.block.stmts.insert(0, check.clone());
                }
                *count += 1;
                Ok(syn::Item::Fn(func))
            }
            syn::Item::Mod(mut m) => {
                if let Some((brace, inner)) = m.content.take() {
                    m.content = Some((brace, mark_tests(tier, inner, reason, env_check, count)?));
                }
                Ok(syn::Item::Mod(m))
            }
            other => Ok(other),
        })
        .collect()
}

/// 带最后一段叫 test 的属性：`#[test]`、`#[tokio::test]`、`#[tokio::test(flavor = ..)]` 等
fn is_test(func: &syn::ItemFn) -> bool {
    func.attrs
        .iter()
        .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
}

fn ignore_reason(tier: Tier, env: &[syn::LitStr]) -> String {
    match tier {
        Tier::Container => {
            "container: needs a container runtime; run with `-- --ignored ::container::`".into()
        }
        Tier::Live if env.is_empty() => {
            "live: needs an external service; run with `-- --ignored ::live::`".into()
        }
        Tier::Live => {
            let names: Vec<String> = env.iter().map(|e| e.value()).collect();
            format!(
                "live: needs env {}; run with `-- --ignored ::live::`",
                names.join(", ")
            )
        }
    }
}

/// 检查环境变量的语句，插在每个测试的函数体开头
fn env_check(krate: &syn::Path, env: &[syn::LitStr]) -> Option<syn::Stmt> {
    if env.is_empty() {
        return None;
    }
    Some(syn::parse_quote! {
        #krate::tier::require_env(&[#(#env),*]);
    })
}

/// 生成代码里引用 hygiea-test 的路径：按调用方 Cargo.toml 里实际的依赖名（包括改名），找不到时退回
/// `::hygiea_test`。在 hygiea-test 自己里面用时也生成 `::hygiea_test`，靠它的 `extern crate self as hygiea_test` 解析
fn krate() -> syn::Path {
    let name = match proc_macro_crate::crate_name("hygiea-test") {
        Ok(proc_macro_crate::FoundCrate::Name(name)) => name,
        Ok(proc_macro_crate::FoundCrate::Itself) | Err(_) => "hygiea_test".to_string(),
    };
    let ident = syn::Ident::new(&name, proc_macro2::Span::call_site());
    syn::parse_quote!(::#ident)
}

/// `#[container]` 不收参数；`#[live]` 收可选的 `env = ["A", "B"]`
fn parse_args(tier: Tier, attr: proc_macro2::TokenStream) -> syn::Result<Vec<syn::LitStr>> {
    let mut env = Vec::new();
    let parser = syn::meta::parser(|meta| {
        if matches!(tier, Tier::Live) && meta.path.is_ident("env") {
            let value = meta.value()?;
            let content;
            syn::bracketed!(content in value);
            let list: Punctuated<syn::LitStr, syn::Token![,]> =
                content.parse_terminated(|p| p.parse::<syn::LitStr>(), syn::Token![,])?;
            env.extend(list);
            Ok(())
        } else {
            let expected = match tier {
                Tier::Container => "#[container] takes no arguments",
                Tier::Live => "expected `env = [\"NAME\", ..]`",
            };
            Err(meta.error(expected))
        }
    });
    syn::parse::Parser::parse2(parser, attr)?;
    Ok(env)
}
