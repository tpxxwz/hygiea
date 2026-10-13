//! 锁的两个 derive，编号规则和错误码一样（前缀部分见 prefix.rs），值是 9 位十进制数，放得进 `i32`：
//! - `#[derive(hy_lock_type)]`：锁类型，`try_lock_i32` 的 `key1`，变体上写 `#[lock_type(code = "..")]`
//! - `#[derive(hy_lock_key)]`：字符串 key，`lock_key!` 用，变体上写 `#[lock_key(code = "..", tpl = "..")]`
//!
//! 两个 derive 共用一套编号和注册表（`LOCK_REGISTRATIONS`），同一个模块里互相也不能重复。重复的拦法和错误码一样：
//! 同一个 enum 里宏报错，同模块靠 const 名字 `HYGIEA_LOCK_<9 位>`（E0428），跨 crate 靠导出符号
//! `__hygiea_lock_<9 位>`（Linux 链接期）和 core 启动时查重

use std::collections::HashMap;

use quote::{format_ident, quote};

use crate::error::{expand_err, expand_err_span, is_numeric_with_len};
use crate::prefix::{Names, Resolved};

/// 完整编号的位数：`i32` 最大 2147483647，9 位的都放得下
const VALUE_LEN: usize = 9;

const LOCK_TYPE: Names = Names {
    derive: "hy_lock_type",
    variant_attr: "lock_type",
    module: "lock_module_prefix",
    internal: "lock_internal_module_prefix",
};

const LOCK_KEY: Names = Names {
    derive: "hy_lock_key",
    variant_attr: "lock_key",
    module: "lock_module_prefix",
    internal: "lock_internal_module_prefix",
};

pub(crate) fn derive_lock_type(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let ast = syn::parse_macro_input!(input as syn::DeriveInput);
    expand(&ast, &LOCK_TYPE).unwrap_or_else(|e| e).into()
}

pub(crate) fn derive_lock_key(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let ast = syn::parse_macro_input!(input as syn::DeriveInput);
    expand(&ast, &LOCK_KEY).unwrap_or_else(|e| e).into()
}

/// 一个变体解析完的结果
struct LockVariant {
    var_name: syn::Ident,
    /// 9 位完整编号
    full: String,
    value: i32,
    /// `hy_lock_key` 的模板和它要的变量
    tpl: Option<(String, Vec<String>)>,
}

fn expand(
    ast: &syn::DeriveInput,
    names: &Names,
) -> Result<proc_macro2::TokenStream, proc_macro2::TokenStream> {
    let resolved = crate::prefix::resolve(ast, names, VALUE_LEN)?;
    let with_tpl = names.variant_attr == LOCK_KEY.variant_attr;

    let mut seen: HashMap<i32, syn::Ident> = HashMap::new();
    let mut variants = Vec::new();
    for variant in resolved.variants {
        crate::prefix::check_variant(variant, names)?;
        let parsed = parse_variant(variant, names, &resolved, with_tpl)?;
        if let Some(first) = seen.get(&parsed.0.value) {
            return Err(expand_err(
                parsed.1,
                &format!(
                    "duplicate lock code {}; first used by variant `{first}`",
                    parsed.0.full
                ),
            ));
        }
        seen.insert(parsed.0.value, parsed.0.var_name.clone());
        variants.push(parsed.0);
    }

    let enum_name = &ast.ident;
    let krate = &resolved.krate;
    let registrations = variants.iter().map(|v| registration(enum_name, v, krate));
    let impls = if with_tpl {
        lock_key_impl(enum_name, &variants, krate)
    } else {
        lock_type_impl(enum_name, &variants, krate)
    };
    let tracking = resolved.tracking();
    Ok(quote! {
        #impls
        #(#registrations)*
        #tracking
    })
}

/// 读变体上的 `#[lock_type(code = "..")]` / `#[lock_key(code = "..", tpl = "..")]`，返回解析结果和 code 的位置
fn parse_variant(
    variant: &syn::Variant,
    names: &Names,
    resolved: &Resolved,
    with_tpl: bool,
) -> Result<(LockVariant, proc_macro2::Span), proc_macro2::TokenStream> {
    let var_name = &variant.ident;
    let expected = if with_tpl {
        "expected `code = \"..\"` or `tpl = \"..\"`"
    } else {
        "expected `code = \"..\"`"
    };
    let mut code: Option<syn::LitStr> = None;
    let mut tpl: Option<syn::LitStr> = None;
    for attr in &variant.attrs {
        if !attr.path().is_ident(names.variant_attr) {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let slot = if meta.path.is_ident("code") {
                &mut code
            } else if with_tpl && meta.path.is_ident("tpl") {
                &mut tpl
            } else {
                return Err(meta.error(format!(
                    "unknown `{}` option, {expected}",
                    names.variant_attr
                )));
            };
            if slot.is_some() {
                return Err(meta.error("duplicate option"));
            }
            *slot = Some(meta.value()?.parse()?);
            Ok(())
        })
        .map_err(|e| e.to_compile_error())?;
    }

    let Some(code) = code else {
        return Err(expand_err_span(
            var_name,
            &format!("`code` missing in `#[{}(..)]`", names.variant_attr),
        ));
    };
    let raw = code.value();
    if !is_numeric_with_len(&raw, resolved.code_len) {
        return Err(expand_err(
            code.span(),
            &format!(
                "lock code must be exactly {} digits here",
                resolved.code_len
            ),
        ));
    }
    let full = format!("{}{raw}", resolved.head);
    // 9 位纯数字都放得进 i32，解析失败只可能是位数算错了
    let value = full
        .parse::<i32>()
        .map_err(|_| expand_err(code.span(), "lock code does not fit in i32"))?;

    let tpl = if with_tpl {
        let Some(tpl) = tpl else {
            return Err(expand_err_span(
                var_name,
                "`tpl` missing in `#[lock_key(..)]`",
            ));
        };
        let vars = crate::template::template_vars(&tpl.value(), tpl.span(), "tpl")?;
        Some((tpl.value(), vars))
    } else {
        None
    };

    Ok((
        LockVariant {
            var_name: var_name.clone(),
            full,
            value,
            tpl,
        },
        code.span(),
    ))
}

/// 同模块重复的编译期保护、跨 crate 的链接保护、注册项
fn registration(
    enum_name: &syn::Ident,
    v: &LockVariant,
    krate: &syn::Path,
) -> proc_macro2::TokenStream {
    let full = &v.full;
    let guard = format_ident!("HYGIEA_LOCK_{}", full, span = v.var_name.span());
    let symbol = format!("__hygiea_lock_{full}");
    let name = format!("{enum_name}::{}", v.var_name);
    let tpl = match &v.tpl {
        Some((tpl, _)) => quote! { ::core::option::Option::Some(#tpl) },
        None => quote! { ::core::option::Option::None },
    };
    quote! {
        #[allow(dead_code)]
        const #guard: () = ();
        const _: () = {
            #[used]
            #[unsafe(export_name = #symbol)]
            static LINK_GUARD: u8 = 0;

            // 路径都经 ::hygiea::__private 转接，下游只依赖 hygiea 就够了，不用自己再加 linkme
            #[#krate::__private::linkme::distributed_slice(#krate::lock::__private::LOCK_REGISTRATIONS)]
            #[linkme(crate = #krate::__private::linkme)]
            static REGISTRATION: #krate::lock::__private::LockRegistration =
                #krate::lock::__private::LockRegistration {
                    code: #full,
                    name: #name,
                    tpl: #tpl,
                };
        };
    }
}

fn lock_type_impl(
    enum_name: &syn::Ident,
    variants: &[LockVariant],
    krate: &syn::Path,
) -> proc_macro2::TokenStream {
    let arms = variants.iter().map(|v| {
        let (var_name, value) = (&v.var_name, v.value);
        quote! { #enum_name::#var_name => #krate::lock::__private::registered(#value) }
    });
    quote! {
        impl ::core::convert::From<#enum_name> for #krate::lock::LockType {
            fn from(value: #enum_name) -> Self {
                match value {
                    #(#arms,)*
                }
            }
        }
    }
}

fn lock_key_impl(
    enum_name: &syn::Ident,
    variants: &[LockVariant],
    krate: &syn::Path,
) -> proc_macro2::TokenStream {
    let vars_arms = variants.iter().map(|v| {
        let var_name = &v.var_name;
        let vars = v
            .tpl
            .as_ref()
            .map(|(_, vars)| vars.as_slice())
            .unwrap_or_default();
        quote! { #enum_name::#var_name => &[#(#vars),*] }
    });
    let key_arms = variants.iter().map(|v| {
        let (var_name, full) = (&v.var_name, &v.full);
        quote! { #enum_name::#var_name => #krate::lock::__private::lock_key(#full, args) }
    });
    quote! {
        impl #enum_name {
            /// 模板需要的变量名（已排序）。lock_key! 在 const 块里调用它做编译期检查，
            /// 单变量简写 lock_key!(X, 值) 也靠它取 key
            #[doc(hidden)]
            pub const fn __hygiea_tpl_vars(&self) -> &'static [&'static str] {
                match self {
                    #(#vars_arms,)*
                }
            }

            /// lock_key! 展开用，外部请用 lock_key!：写法用错时它在编译期报错
            #[doc(hidden)]
            pub fn __hygiea_lock_key(
                &self,
                args: ::core::option::Option<#krate::__private::serde_json::Value>,
            ) -> #krate::lock::LockKey {
                match self {
                    #(#key_arms,)*
                }
            }
        }
    }
}
