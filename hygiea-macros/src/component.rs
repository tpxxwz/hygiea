use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{ImplItem, ItemImpl};

// #[component] 属性宏的实现。
//
// 组件要实现两个 trait：公共的 Component（Config、build、provides…、Kind）和启动方式对应的
// ImmediateComponent / DeferredComponent。宏让这些写在一个 impl 块里，按项的名字拆成两个 impl：
//
// #[component]
// impl ImmediateComponent for PgComponent {
//     type Config = PgConfig;
//     fn build(..) -> Self { .. }
//     async fn startup(..) { .. }
// }
//
// 展开成（路径按调用方的依赖名来，见 krate.rs）：
//
// #[::hygiea::app::async_trait]
// impl ::hygiea::app::Component for PgComponent {
//     type Kind = ::hygiea::app::Immediate;      // DeferredComponent 对应 Deferred
//     type Config = PgConfig;
//     fn build(..) -> Self { .. }
// }
// #[::hygiea::app::async_trait]
// impl ImmediateComponent for PgComponent {
//     async fn startup(..) { .. }
// }
//
// 不属于 Component 的项原样留在启动 trait 的 impl 里：写错名字（provide）、写了另一种启动方式的方法
// （ImmediateComponent 里写 activate）都由编译器在用户写的那一行报 "not a member of trait"。

/// 属于 Component 的项，按名字挪进 `impl Component`
const COMPONENT_ITEMS: &[&str] = &[
    "Config",
    "build",
    "provides",
    "depends_on",
    "stop",
    "shutdown_timeout",
];

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    // 唯一的参数是 `#[component(crate = "path")]`，显式指定生成代码里引用 hygiea 的路径
    let mut explicit_crate: Option<syn::Path> = None;
    let parser = syn::meta::parser(|meta| {
        if crate::krate::parse_crate_arg(&meta, &mut explicit_crate)? {
            Ok(())
        } else {
            Err(meta.error("`#[component]` only takes `crate = \"path\"`"))
        }
    });
    if let Err(e) = syn::parse::Parser::parse(parser, attr) {
        return e.to_compile_error().into();
    }
    let krate = crate::krate::resolve(explicit_crate);
    let ast = syn::parse_macro_input!(item as ItemImpl);
    expand_impl(ast, &krate)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

fn expand_impl(input: ItemImpl, krate: &syn::Path) -> syn::Result<proc_macro2::TokenStream> {
    const WRONG_TRAIT: &str = "`#[component]` must be on `impl ImmediateComponent for ..` or `impl DeferredComponent for ..`";
    let Some((trait_path, _)) = &input.trait_ else {
        return Err(syn::Error::new(input.self_ty.span(), WRONG_TRAIT));
    };
    // 按 trait 名最后一段判断，`app::ImmediateComponent` 这样带路径写也行
    let kind = match trait_path.segments.last().map(|seg| seg.ident.to_string()) {
        Some(name) if name == "ImmediateComponent" => quote!(#krate::app::Immediate),
        Some(name) if name == "DeferredComponent" => quote!(#krate::app::Deferred),
        _ => return Err(syn::Error::new(trait_path.span(), WRONG_TRAIT)),
    };

    let mut component_items: Vec<ImplItem> = Vec::new();
    let mut startup_items: Vec<ImplItem> = Vec::new();
    for item in input.items {
        let name = match &item {
            ImplItem::Type(t) => Some(&t.ident),
            ImplItem::Fn(f) => Some(&f.sig.ident),
            _ => None,
        };
        match name {
            Some(ident) if ident == "Kind" => {
                return Err(syn::Error::new(
                    ident.span(),
                    "`type Kind` is set by `#[component]` from the trait, remove it",
                ));
            }
            Some(ident) if COMPONENT_ITEMS.iter().any(|n| ident == n) => component_items.push(item),
            _ => startup_items.push(item),
        }
    }

    let (impl_generics, _, where_clause) = input.generics.split_for_impl();
    let self_ty = &input.self_ty;
    let attrs = &input.attrs;
    // 漏写 build / startup 这类必需项时，报错落在 `#[component]` 这一行（"missing `build`"）：
    // async_trait 重新生成 impl 时用的是调用处的 span，这里设 span 也改不了
    Ok(quote! {
        #[#krate::app::async_trait]
        impl #impl_generics #krate::app::Component for #self_ty #where_clause {
            type Kind = #kind;
            #(#component_items)*
        }

        #(#attrs)*
        #[#krate::app::async_trait]
        impl #impl_generics #trait_path for #self_ty #where_clause {
            #(#startup_items)*
        }
    })
}
