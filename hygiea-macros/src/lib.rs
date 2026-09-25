use proc_macro::TokenStream;

mod error;
mod krate;
mod redact;

#[proc_macro_derive(hy_err, attributes(err_code_module_prefix, error, hy_err))]
pub fn derive_hy_err(input: TokenStream) -> TokenStream {
    error::derive::<error::HyVariant>(input, "hy_err")
}

/// 字段上标 `#[redact(mask)]` / `#[redact(skip)]`，让它在日志里打码或不出现，平时照常序列化。
/// 必须写在 `#[derive(Serialize)]` 上面，见 `hygiea::redact`
#[proc_macro_attribute]
pub fn redact(attr: TokenStream, item: TokenStream) -> TokenStream {
    redact::expand(attr, item)
}
