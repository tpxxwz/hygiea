//! `#[derive(hy_live_config)]`：给动态配置的 struct 生成按字段读的句柄（实现 hygiea-db 的 `live_config::LiveConfigFields`）。
//!
//! ```ignore
//! #[derive(hy_live_config)]
//! pub struct MarkjiSync {
//!     pub start_time: String,
//!     #[live_config(by_ref)]
//!     pub levels: Vec<i32>,
//!     #[live_config(nested)]
//!     pub retry: Retry,
//! }
//! ```
//!
//! 展开成（路径按调用方的依赖名来，见 krate.rs）：
//!
//! ```ignore
//! #[derive(Clone)]
//! pub struct MarkjiSyncLiveConfig { view: LiveView<MarkjiSync> }
//!
//! impl LiveConfigFields for MarkjiSync { type Handle = MarkjiSyncLiveConfig; fn handle(view) -> MarkjiSyncLiveConfig { .. } }
//!
//! impl MarkjiSyncLiveConfig {
//!     pub fn snapshot(&self) -> LiveRef<MarkjiSync>;           // 还有 changed / changed_by / on_change / on_change_by
//!     pub fn start_time(&self) -> String { self.view.read(|c| Clone::clone(&c.start_time)) }
//!     pub fn levels(&self) -> LiveRef<Vec<i32>> { self.view.get().map(|c| &c.levels) }
//!     pub fn retry(&self) -> <Retry as LiveConfigFields>::Handle { <Retry as LiveConfigFields>::handle(self.view.view(|c| &c.retry)) }
//! }
//! ```

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// 句柄上固定生成的方法，字段名不能和它们重名。`clone` 是 `#[derive(Clone)]` 的方法，
/// 字段方法叫 `clone` 会让 `handle.clone()` 调到字段方法上
const RESERVED: &[&str] = &[
    "snapshot",
    "changed",
    "changed_by",
    "on_change",
    "on_change_by",
    "clone",
];

/// 字段上的 `#[live_config(..)]`
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// 不标：返回字段值的 clone
    Clone,
    /// `#[live_config(by_ref)]`：返回 `LiveRef`
    ByRef,
    /// `#[live_config(nested)]`：返回字段类型的句柄
    Nested,
}

pub(crate) fn derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let ast = syn::parse_macro_input!(input as syn::DeriveInput);
    expand(ast).unwrap_or_else(|e| e.to_compile_error()).into()
}

fn expand(ast: syn::DeriveInput) -> syn::Result<TokenStream> {
    let explicit_crate = container_attr(&ast.attrs)?;
    let fields = match &ast.data {
        syn::Data::Struct(s) => match &s.fields {
            syn::Fields::Named(named) => &named.named,
            syn::Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    &ast.ident,
                    "`#[derive(hy_live_config)]` only supports structs with named fields, not tuple structs",
                ));
            }
            syn::Fields::Unit => {
                return Err(syn::Error::new_spanned(
                    &ast.ident,
                    "`#[derive(hy_live_config)]` only supports structs with named fields, not unit structs",
                ));
            }
        },
        syn::Data::Enum(_) | syn::Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                &ast.ident,
                "`#[derive(hy_live_config)]` only supports structs with named fields",
            ));
        }
    };
    if !ast.generics.params.is_empty() || ast.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &ast.generics,
            "`#[derive(hy_live_config)]` does not support generic structs",
        ));
    }

    let live = crate::krate::resolve_live_config(explicit_crate);
    let name = &ast.ident;
    let vis = &ast.vis;
    let handle = format_ident!("{}LiveConfig", name);

    let mut methods = Vec::with_capacity(fields.len());
    for field in fields {
        methods.push(field_method(field, &live)?);
    }

    let struct_doc = format!(
        " [`{name}`] 的动态配置句柄，`#[derive(hy_live_config)]` 生成：每个字段一个同名方法，每次调用读到最新的值；\
         `snapshot()` 是整块的快照。`Clone` 后各处共用。\n\n\
         连续调几个字段方法，中间配置可能换了版本，几个值要来自同一版时用 `snapshot()`"
    );
    let snapshot_doc =
        format!(" 整块 [`{name}`] 当前最新的快照，拿着期间配置换了新值也不影响手上这份");

    Ok(quote! {
        #[doc = #struct_doc]
        #[derive(::core::clone::Clone)]
        #vis struct #handle {
            view: #live::LiveView<#name>,
        }

        impl #live::LiveConfigFields for #name {
            type Handle = #handle;

            fn handle(view: #live::LiveView<Self>) -> #handle {
                #handle { view }
            }
        }

        impl #handle {
            #[doc = #snapshot_doc]
            #vis fn snapshot(&self) -> #live::LiveRef<#name> {
                self.view.get()
            }

            /// 等到整份配置变了再返回，见 `LiveView::changed`
            #vis async fn changed(&mut self) -> #live::__private::Result<()> {
                self.view.changed().await
            }

            /// 等到 `project` 取出的那个字段变了再返回，见 `LiveView::changed_by`
            #vis async fn changed_by<V: ::core::cmp::PartialEq>(
                &mut self,
                project: fn(&#name) -> &V,
            ) -> #live::__private::Result<()> {
                self.view.changed_by(project).await
            }

            /// 整份配置变了就调一次 `f`，drop 掉返回的 guard 就停，见 `LiveView::on_change`
            #vis fn on_change(
                &self,
                f: impl ::core::ops::Fn(&#name) + ::core::marker::Send + ::core::marker::Sync + 'static,
            ) -> #live::OnChangeGuard {
                self.view.on_change(f)
            }

            /// `project` 取出的那个字段变了才调 `f`，drop 掉返回的 guard 就停，见 `LiveView::on_change_by`
            #vis fn on_change_by<V: ::core::cmp::PartialEq + 'static>(
                &self,
                project: fn(&#name) -> &V,
                f: impl ::core::ops::Fn(&V) + ::core::marker::Send + ::core::marker::Sync + 'static,
            ) -> #live::OnChangeGuard {
                self.view.on_change_by(project, f)
            }

            #(#methods)*
        }
    })
}

/// struct 上的 `#[hy_live_config(crate = "..")]`；字段属性 `#[live_config(..)]` 写到 struct 上时报错
fn container_attr(attrs: &[syn::Attribute]) -> syn::Result<Option<syn::Path>> {
    if let Some(attr) = attrs.iter().find(|a| a.path().is_ident("live_config")) {
        return Err(syn::Error::new_spanned(
            attr,
            "`#[live_config(..)]` goes on fields (`nested` / `by_ref`); on the struct use `#[hy_live_config(crate = \"path\")]`",
        ));
    }
    let mut explicit_crate = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident("hy_live_config")) {
        attr.parse_nested_meta(|meta| {
            if crate::krate::parse_crate_arg(&meta, &mut explicit_crate)? {
                return Ok(());
            }
            Err(meta.error("unknown `hy_live_config` attribute; expected `crate = \"path\"`"))
        })?;
    }
    Ok(explicit_crate)
}

/// 字段上的 `#[live_config(nested)]` / `#[live_config(by_ref)]`
fn field_mode(field: &syn::Field) -> syn::Result<Mode> {
    let mut nested = None;
    let mut by_ref = None;
    for attr in field
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("live_config"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("nested") {
                nested = Some(meta.path.clone());
                Ok(())
            } else if meta.path.is_ident("by_ref") {
                by_ref = Some(meta.path.clone());
                Ok(())
            } else {
                Err(meta.error(
                    "unknown `live_config` attribute on a field; expected `nested` or `by_ref`",
                ))
            }
        })?;
    }
    match (nested, by_ref) {
        (Some(_), Some(by_ref)) => Err(syn::Error::new_spanned(
            by_ref,
            "`#[live_config(nested)]` and `#[live_config(by_ref)]` cannot be used together: \
             `nested` returns the field's own handle, `by_ref` returns a `LiveRef` snapshot",
        )),
        (Some(_), None) => Ok(Mode::Nested),
        (None, Some(_)) => Ok(Mode::ByRef),
        (None, None) => Ok(Mode::Clone),
    }
}

fn field_method(field: &syn::Field, live: &syn::Path) -> syn::Result<TokenStream> {
    let mode = field_mode(field)?;
    // 具名字段一定有名字，这里只是不用 unwrap
    let Some(ident) = &field.ident else {
        return Err(syn::Error::new_spanned(
            field,
            "`#[derive(hy_live_config)]` expects a named field",
        ));
    };
    let plain = ident.to_string();
    let plain = plain.strip_prefix("r#").unwrap_or(&plain);
    if RESERVED.contains(&plain) {
        return Err(syn::Error::new_spanned(
            ident,
            format!(
                "field `{plain}` conflicts with the method `{plain}` generated by `#[derive(hy_live_config)]` \
                 (reserved: {}); rename the field and keep the key with `#[serde(rename = \"{plain}\")]`",
                RESERVED.join(", ")
            ),
        ));
    }
    let vis = &field.vis;
    let ty = &field.ty;
    // 字段自己的文档接在生成的说明后面
    let field_docs = field.attrs.iter().filter(|a| a.path().is_ident("doc"));
    Ok(match mode {
        Mode::Clone => {
            let doc =
                format!(" 字段 `{plain}` 当前最新的值（clone 一份），每次调用都读最新的那一版");
            quote! {
                #[doc = #doc]
                #[doc = ""]
                #(#field_docs)*
                #vis fn #ident(&self) -> #ty {
                    self.view.read(|c| ::core::clone::Clone::clone(&c.#ident))
                }
            }
        }
        Mode::ByRef => {
            let doc = format!(
                " 字段 `{plain}` 当前最新的快照，不复制值，拿着期间配置换了新值也不影响手上这份"
            );
            quote! {
                #[doc = #doc]
                #[doc = ""]
                #(#field_docs)*
                #vis fn #ident(&self) -> #live::LiveRef<#ty> {
                    self.view.get().map(|c| &c.#ident)
                }
            }
        }
        Mode::Nested => {
            let doc = format!(" 字段 `{plain}` 的句柄，接着按字段读，每次读到最新的值");
            quote! {
                #[doc = #doc]
                #[doc = ""]
                #(#field_docs)*
                #vis fn #ident(&self) -> <#ty as #live::LiveConfigFields>::Handle {
                    <#ty as #live::LiveConfigFields>::handle(self.view.view(|c| &c.#ident))
                }
            }
        }
    })
}
