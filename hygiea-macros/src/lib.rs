use proc_macro::TokenStream;

#[cfg(feature = "component")]
mod component;
mod error;
mod krate;
#[cfg(feature = "lock")]
mod lock;
mod prefix;
#[cfg(feature = "redact")]
mod redact;
mod template;

#[proc_macro_derive(
    hy_err,
    attributes(err_code_module_prefix, err_code_internal_module_prefix, error, hy_err)
)]
pub fn derive_hy_err(input: TokenStream) -> TokenStream {
    error::derive::<error::HyVariant>(input)
}

/// 注册锁类型（`try_lock_i32` 的 `key1`），编号的组成和查重规则和错误码一样，见 `hygiea::lock`
#[cfg(feature = "lock")]
#[proc_macro_derive(
    hy_lock_type,
    attributes(
        lock_module_prefix,
        lock_internal_module_prefix,
        lock_type,
        hy_lock_type
    )
)]
pub fn derive_hy_lock_type(input: TokenStream) -> TokenStream {
    lock::derive_lock_type(input)
}

/// 注册字符串锁 key 的模板，用 `lock_key!` 生成 key；编号和锁类型共用，见 `hygiea::lock`
#[cfg(feature = "lock")]
#[proc_macro_derive(
    hy_lock_key,
    attributes(lock_module_prefix, lock_internal_module_prefix, lock_key, hy_lock_key)
)]
pub fn derive_hy_lock_key(input: TokenStream) -> TokenStream {
    lock::derive_lock_key(input)
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
