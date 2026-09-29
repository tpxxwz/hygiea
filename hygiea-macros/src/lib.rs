use proc_macro::TokenStream;

#[cfg(feature = "component")]
mod component;
mod error;
mod krate;
#[cfg(feature = "redact")]
mod redact;

#[proc_macro_derive(
    hy_err,
    attributes(err_code_module_prefix, err_code_internal_module_prefix, error, hy_err)
)]
pub fn derive_hy_err(input: TokenStream) -> TokenStream {
    error::derive::<error::HyVariant>(input, "hy_err")
}

/// 字段上标 `#[redact(mask)]` / `#[redact(skip)]`，让它在日志里打码或不出现，平时照常序列化。
/// 必须写在 `#[derive(Serialize)]` 上面，见 `hygiea::redact`
#[cfg(feature = "redact")]
#[proc_macro_attribute]
pub fn redact(attr: TokenStream, item: TokenStream) -> TokenStream {
    redact::expand(attr, item)
}

/// 标在 `impl ImmediateComponent for ..` / `impl DeferredComponent for ..` 上，把 `Component` 的项和启动方法
/// 写在一个 impl 块里，展开成两个 impl，见 `hygiea::app::component`
#[cfg(feature = "component")]
#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    component::expand(attr, item)
}
