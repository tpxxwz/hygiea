use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;

// #[redact] 属性宏的实现。
//
// 打码做在 Serialize 里：宏把字段上的 #[redact(mask)] / #[redact(skip)] 换成 serde 属性，
// 让这个字段在「日志模式」下输出 "***" 或者不出现，平时照常序列化。日志模式是
// hygiea::redact 里的线程局部开关，只在生成日志预览那一次序列化期间打开。
//
// #[redact]
// #[derive(Serialize)]
// struct LoginReq {
//     #[redact(mask)]
//     password: String,
// }
//
// 展开成：
//
// #[derive(Serialize)]
// struct LoginReq {
//     #[serde(serialize_with = "::hygiea::redact::mask")]   // 路径按调用方的依赖名来，见 krate.rs
//     password: String,
// }
//
// 因为走的是 Serialize，serde 序列化到哪一层打码就跟到哪一层：Vec、HashMap、Option、泛型外层、
// 第三方容器里的元素都一样，不需要额外标记，也不需要按 key 去匹配字段。
//
// 字段上已经有 serialize_with / with / skip_serializing_if 时不能再加一份（serde 会报重复），
// 这时生成一个组合用的辅助函数：日志模式下打码，否则调用户原来的那个。辅助函数放在类型自己的
// inherent impl 里，serde 生成的代码里泛型参数同名可见，所以路径写成 `Type::<T>::helper`。
//
// 必须写在 #[derive(Serialize)] 上面：属性宏要在 serde 的 derive 之前改写字段。
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    // 唯一的参数是 `#[redact(crate = "path")]`，显式指定生成代码里引用 hygiea 的路径
    let mut explicit_crate: Option<syn::Path> = None;
    let parser = syn::meta::parser(|meta| {
        if crate::krate::parse_crate_arg(&meta, &mut explicit_crate)? {
            Ok(())
        } else {
            Err(meta.error("`#[redact]` only takes `crate = \"path\"`"))
        }
    });
    if let Err(e) = syn::parse::Parser::parse(parser, attr) {
        return e.to_compile_error().into();
    }
    let krate = crate::krate::resolve(explicit_crate);
    let ast = syn::parse_macro_input!(item as syn::DeriveInput);
    expand_item(ast, &krate)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Mask,
    Skip,
}

fn expand_item(
    mut ast: syn::DeriveInput,
    krate: &syn::Path,
) -> syn::Result<proc_macro2::TokenStream> {
    // serde 属性里的函数路径是字符串，这里拼好；quote 生成的代码里直接插 Path
    let redact_str = format!("{}::redact", quote!(#krate)).replace(' ', "");
    reject_redact_attr(&ast.attrs, "`#[redact(..)]` is only allowed on fields")?;
    let container = serde_metas(&ast.attrs)?;
    let into = container
        .iter()
        .find(|m| m.path().is_ident("into"))
        .cloned();

    // 写在 #[derive(Serialize)] 下面时 derive 已经先展开了，这里改写字段已经晚了，打码会悄悄失效；
    // 只 derive 了 Deserialize 的类型标 #[redact(..)] 也没有意义。两种情况这里都看不到 derive(Serialize)
    let derives_serialize = derives_serialize(&ast.attrs)?;

    let name = ast.ident.clone();
    let helper_prefix = helper_path_prefix(&name, &ast.generics);
    let mut helpers = Vec::new();
    let mut counter = 0usize;

    // 每个字段连同「它所在的 variant 上有没有整体接管序列化的 serde 属性」一起处理
    let mut visit = |field: &mut syn::Field, variant_override: Option<&syn::Meta>| {
        let Some((action, redact_attr)) = take_redact_attr(&mut field.attrs)? else {
            return Ok(());
        };
        if let Some(meta) = &into {
            return Err(syn::Error::new_spanned(
                meta,
                "`#[redact(..)]` has no effect with `#[serde(into = ..)]`: the target type is \
                 serialized instead, mark its fields there",
            ));
        }
        if let Some(meta) = variant_override {
            return Err(syn::Error::new_spanned(
                meta,
                "`#[redact(..)]` has no effect here: this variant is serialized by its own \
                 `serialize_with` / `with`, mask inside that function instead",
            ));
        }
        if !derives_serialize {
            return Err(syn::Error::new_spanned(
                &redact_attr,
                "`#[redact(..)]` has no effect: `#[redact]` must be placed above \
                 `#[derive(Serialize)]` on a type that derives `Serialize`",
            ));
        }
        counter += 1;
        rewrite_field(
            field,
            &redact_str,
            action,
            &redact_attr,
            counter,
            &helper_prefix,
            &mut helpers,
        )
    };

    match &mut ast.data {
        syn::Data::Struct(s) => {
            for field in s.fields.iter_mut() {
                visit(field, None)?;
            }
        }
        syn::Data::Enum(e) => {
            for variant in e.variants.iter_mut() {
                reject_redact_attr(
                    &variant.attrs,
                    "`#[redact(..)]` is only allowed on fields, not on enum variants",
                )?;
                let metas = serde_metas(&variant.attrs)?;
                let override_meta = metas.into_iter().find(|m| {
                    m.path().is_ident("serialize_with")
                        || m.path().is_ident("with")
                        || m.path().is_ident("skip")
                        || m.path().is_ident("skip_serializing")
                });
                for field in variant.fields.iter_mut() {
                    visit(field, override_meta.as_ref())?;
                }
            }
        }
        syn::Data::Union(u) => {
            for field in u.fields.named.iter() {
                reject_redact_attr(&field.attrs, "`#[redact(..)]` is not supported on unions")?;
            }
        }
    }

    let helper_impl = if helpers.is_empty() {
        quote! {}
    } else {
        let (impl_generics, ty_generics, where_clause) = ast.generics.split_for_impl();
        quote! {
            #[doc(hidden)]
            #[allow(non_snake_case)]
            impl #impl_generics #name #ty_generics #where_clause {
                #(#helpers)*
            }
        }
    };

    Ok(quote! {
        #ast
        #helper_impl
    })
}

/// 属性里有没有 `#[derive(.., Serialize, ..)]`，`serde::Serialize` 这类带路径的写法也算
fn derives_serialize(attrs: &[syn::Attribute]) -> syn::Result<bool> {
    for attr in attrs.iter().filter(|a| a.path().is_ident("derive")) {
        let paths =
            attr.parse_args_with(Punctuated::<syn::Path, syn::Token![,]>::parse_terminated)?;
        if paths
            .iter()
            .any(|p| p.segments.last().is_some_and(|s| s.ident == "Serialize"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `Type::<'a, T, N>::`，给 serde 属性里的辅助函数路径用
fn helper_path_prefix(name: &syn::Ident, generics: &syn::Generics) -> String {
    let args: Vec<String> = generics
        .params
        .iter()
        .map(|p| match p {
            syn::GenericParam::Lifetime(l) => l.lifetime.to_string(),
            syn::GenericParam::Type(t) => t.ident.to_string(),
            syn::GenericParam::Const(c) => c.ident.to_string(),
        })
        .collect();
    if args.is_empty() {
        format!("{name}::")
    } else {
        format!("{name}::<{}>::", args.join(", "))
    }
}

fn reject_redact_attr(attrs: &[syn::Attribute], msg: &str) -> syn::Result<()> {
    match attrs.iter().find(|a| a.path().is_ident("redact")) {
        Some(attr) => Err(syn::Error::new_spanned(attr, msg)),
        None => Ok(()),
    }
}

/// 取出并删掉字段上的 `#[redact(..)]`，只接受 `mask` / `skip` 之一，一个字段只能有一个
fn take_redact_attr(
    attrs: &mut Vec<syn::Attribute>,
) -> syn::Result<Option<(Action, syn::Attribute)>> {
    let mut found: Option<(Action, syn::Attribute)> = None;
    let mut rest = Vec::with_capacity(attrs.len());
    for attr in attrs.drain(..) {
        if !attr.path().is_ident("redact") {
            rest.push(attr);
            continue;
        }
        let mut this: Option<Action> = None;
        attr.parse_nested_meta(|meta| {
            let action = if meta.path.is_ident("mask") {
                Action::Mask
            } else if meta.path.is_ident("skip") {
                Action::Skip
            } else {
                return Err(meta.error("unknown `redact` option, expected `mask` or `skip`"));
            };
            if this.is_some() {
                return Err(meta.error("only one of `mask` / `skip` is allowed"));
            }
            this = Some(action);
            Ok(())
        })?;
        let Some(action) = this else {
            return Err(syn::Error::new_spanned(
                &attr,
                "expected `#[redact(mask)]` or `#[redact(skip)]`",
            ));
        };
        if found.is_some() {
            return Err(syn::Error::new_spanned(
                &attr,
                "duplicate `redact` attribute on this field",
            ));
        }
        found = Some((action, attr));
    }
    *attrs = rest;
    Ok(found)
}

fn rewrite_field(
    field: &mut syn::Field,
    redact_str: &str,
    action: Action,
    redact_attr: &syn::Attribute,
    index: usize,
    helper_prefix: &str,
    helpers: &mut Vec<proc_macro2::TokenStream>,
) -> syn::Result<()> {
    // 字段上所有 #[serde(..)] 拆成单项，要替换的项去掉，最后合成一个新的 #[serde(..)]
    let metas = serde_metas(&field.attrs)?;
    field.attrs.retain(|a| !a.path().is_ident("serde"));

    for meta in &metas {
        let path = meta.path();
        if path.is_ident("skip") || path.is_ident("skip_serializing") {
            return Err(syn::Error::new_spanned(
                redact_attr,
                "this field is already skipped by serde, `#[redact(..)]` has no effect",
            ));
        }
        if action == Action::Mask && path.is_ident("flatten") {
            return Err(syn::Error::new_spanned(
                redact_attr,
                "`#[redact(mask)]` can't be used on a `#[serde(flatten)]` field: a flattened field \
                 must serialize as a map, not a string. Mark the fields of the inner type \
                 instead, or use `#[redact(skip)]`",
            ));
        }
    }

    let ty = &field.ty;
    let redact: syn::Path = syn::parse_str(redact_str)?;
    let mut kept: Vec<syn::Meta> = Vec::with_capacity(metas.len() + 1);
    let mut user: Option<syn::ExprPath> = None;

    match action {
        Action::Mask => {
            // 用户原来的序列化方式：serialize_with = "f"，或 with = "m"（反序列化那半要留下）
            for meta in metas {
                if meta.path().is_ident("serialize_with") {
                    user = Some(parse_path_value(&meta)?);
                } else if meta.path().is_ident("with") {
                    let module = lit_str(&meta)?.value();
                    user = Some(syn::parse_str(&format!("{module}::serialize"))?);
                    let de = format!("{module}::deserialize");
                    kept.push(syn::parse_quote!(deserialize_with = #de));
                } else {
                    kept.push(meta);
                }
            }
            let path = match user {
                None => format!("{redact_str}::mask"),
                Some(user) => {
                    let helper = format_ident!("__hygiea_redact_mask_{}", index);
                    helpers.push(quote! {
                        fn #helper<__S: #redact::__Serializer>(
                            value: &#ty,
                            serializer: __S,
                        ) -> ::core::result::Result<__S::Ok, __S::Error> {
                            if #redact::active() {
                                #redact::write_masked(serializer)
                            } else {
                                #user(value, serializer)
                            }
                        }
                    });
                    format!("{helper_prefix}{helper}")
                }
            };
            kept.push(syn::parse_quote!(serialize_with = #path));
        }
        Action::Skip => {
            for meta in metas {
                if meta.path().is_ident("skip_serializing_if") {
                    user = Some(parse_path_value(&meta)?);
                } else {
                    kept.push(meta);
                }
            }
            let path = match user {
                None => format!("{redact_str}::skip"),
                Some(user) => {
                    let helper = format_ident!("__hygiea_redact_skip_{}", index);
                    helpers.push(quote! {
                        fn #helper(value: &#ty) -> bool {
                            #redact::active() || #user(value)
                        }
                    });
                    format!("{helper_prefix}{helper}")
                }
            };
            kept.push(syn::parse_quote!(skip_serializing_if = #path));
        }
    }

    field.attrs.push(syn::parse_quote!(#[serde(#(#kept),*)]));
    Ok(())
}

fn serde_metas(attrs: &[syn::Attribute]) -> syn::Result<Vec<syn::Meta>> {
    let mut metas = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        let list =
            attr.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)?;
        metas.extend(list);
    }
    Ok(metas)
}

fn lit_str(meta: &syn::Meta) -> syn::Result<syn::LitStr> {
    let syn::Meta::NameValue(nv) = meta else {
        return Err(syn::Error::new_spanned(meta, "expected `name = \"...\"`"));
    };
    match &nv.value {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Ok(s.clone()),
        other => Err(syn::Error::new_spanned(other, "expected a string literal")),
    }
}

fn parse_path_value(meta: &syn::Meta) -> syn::Result<syn::ExprPath> {
    let lit = lit_str(meta)?;
    lit.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== take_redact_attr =====

    #[test]
    fn take_redact_attr_mask() {
        let mut attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[redact(mask)])];
        let (action, _attr) = take_redact_attr(&mut attrs)
            .unwrap()
            .expect("应识别出 mask");
        assert!(matches!(action, Action::Mask));
        assert!(attrs.is_empty());
    }

    #[test]
    fn take_redact_attr_skip() {
        let mut attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[redact(skip)])];
        let (action, _attr) = take_redact_attr(&mut attrs)
            .unwrap()
            .expect("应识别出 skip");
        assert!(matches!(action, Action::Skip));
        assert!(attrs.is_empty());
    }

    #[test]
    fn take_redact_attr_none_when_absent() {
        // 没有 #[redact(..)] 时返回 None，其他属性原样保留
        let mut attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(rename = "x")])];
        let result = take_redact_attr(&mut attrs).unwrap();
        assert!(result.is_none());
        assert_eq!(attrs.len(), 1);
    }

    #[test]
    fn take_redact_attr_keeps_other_attrs() {
        // 取出 #[redact(..)] 之后，字段上别的属性不受影响
        let mut attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[serde(rename = "x")]),
            syn::parse_quote!(#[redact(mask)]),
        ];
        let result = take_redact_attr(&mut attrs).unwrap();
        assert!(result.is_some());
        assert_eq!(attrs.len(), 1);
        assert!(attrs[0].path().is_ident("serde"));
    }

    #[test]
    fn take_redact_attr_empty_is_err() {
        // #[redact()] 里什么都没写
        let mut attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[redact()])];
        assert!(take_redact_attr(&mut attrs).is_err());
    }

    #[test]
    fn take_redact_attr_unknown_option_is_err() {
        let mut attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[redact(hide)])];
        assert!(take_redact_attr(&mut attrs).is_err());
    }

    #[test]
    fn take_redact_attr_duplicate_mask_and_skip_is_err() {
        let mut attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[redact(mask)]),
            syn::parse_quote!(#[redact(skip)]),
        ];
        assert!(take_redact_attr(&mut attrs).is_err());
    }

    #[test]
    fn take_redact_attr_duplicate_within_one_attr_is_err() {
        // 一个 #[redact(..)] 里同时写 mask 和 skip
        let mut attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[redact(mask, skip)])];
        assert!(take_redact_attr(&mut attrs).is_err());
    }

    // ===== serde_metas =====

    #[test]
    fn serde_metas_collects_single_attr() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(rename = "x", skip)])];
        let metas = serde_metas(&attrs).unwrap();
        assert_eq!(metas.len(), 2);
        assert!(metas[0].path().is_ident("rename"));
        assert!(metas[1].path().is_ident("skip"));
    }

    #[test]
    fn serde_metas_merges_multiple_attrs() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[serde(rename = "x")]),
            syn::parse_quote!(#[serde(skip)]),
        ];
        let metas = serde_metas(&attrs).unwrap();
        assert_eq!(metas.len(), 2);
    }

    #[test]
    fn serde_metas_ignores_non_serde_attrs() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[redact(mask)])];
        let metas = serde_metas(&attrs).unwrap();
        assert!(metas.is_empty());
    }

    #[test]
    fn serde_metas_invalid_content_is_err() {
        // 括号里不是合法的 Meta（既不是 path，也不是 name = value / name(..)）
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(1)])];
        assert!(serde_metas(&attrs).is_err());
    }

    // ===== lit_str =====

    #[test]
    fn lit_str_accepts_name_value_string() {
        let meta: syn::Meta = syn::parse_quote!(with = "my_mod");
        let lit = lit_str(&meta).unwrap();
        assert_eq!(lit.value(), "my_mod");
    }

    #[test]
    fn lit_str_rejects_non_string_literal() {
        let meta: syn::Meta = syn::parse_quote!(with = 1);
        assert!(lit_str(&meta).is_err());
    }

    #[test]
    fn lit_str_rejects_non_name_value_meta() {
        // 既不是 name = value，也不是字符串
        let meta: syn::Meta = syn::parse_quote!(skip);
        assert!(lit_str(&meta).is_err());
    }

    // ===== helper_path_prefix =====

    #[test]
    fn helper_path_prefix_no_generics() {
        let name: syn::Ident = syn::parse_quote!(Foo);
        let generics: syn::Generics = syn::parse_quote!();
        assert_eq!(helper_path_prefix(&name, &generics), "Foo::");
    }

    #[test]
    fn helper_path_prefix_single_type_param() {
        let name: syn::Ident = syn::parse_quote!(Foo);
        let generics: syn::Generics = syn::parse_quote!(<T>);
        assert_eq!(helper_path_prefix(&name, &generics), "Foo::<T>::");
    }

    #[test]
    fn helper_path_prefix_lifetime_type_and_const() {
        let name: syn::Ident = syn::parse_quote!(Composed);
        let generics: syn::Generics = syn::parse_quote!(<'a, T, const N: usize>);
        assert_eq!(
            helper_path_prefix(&name, &generics),
            "Composed::<'a, T, N>::"
        );
    }
}
