//! 生成代码里引用 hygiea 的路径。
//!
//! 按调用方的 Cargo.toml 找它实际依赖的名字：依赖了 `hygiea`（包括改名）就用它，否则找 `hygiea-core`；
//! 都找不到时退回 `::hygiea`。宏上显式写了 `crate = "path"` 时以它为准。
//!
//! 被找的就是当前 crate 自己时（比如 hygiea-core 内部），也生成 `::hygiea_core` 这样的绝对路径，
//! 靠 crate 自己的 `extern crate self as ..` 解析：这样库本身、单元测试、集成测试、doctest 用的是同一个路径

use proc_macro_crate::{FoundCrate, crate_name};

/// `explicit` 是宏上写的 `crate = ".."`
pub(crate) fn resolve(explicit: Option<syn::Path>) -> syn::Path {
    if let Some(path) = explicit {
        return path;
    }
    for (package, lib) in [("hygiea", "hygiea"), ("hygiea-core", "hygiea_core")] {
        let name = match crate_name(package) {
            Ok(FoundCrate::Itself) => lib.to_string(),
            Ok(FoundCrate::Name(name)) => name,
            Err(_) => continue,
        };
        // Cargo 的依赖名一定是合法标识符，解析不了就当没找到
        if let Ok(path) = syn::parse_str(&format!("::{name}")) {
            return path;
        }
    }
    syn::parse_quote!(::hygiea)
}

/// 解析 `crate = "path"` 这一项，给 `parse_nested_meta` 用；不是这一项时返回 `Ok(false)`
pub(crate) fn parse_crate_arg(
    meta: &syn::meta::ParseNestedMeta,
    out: &mut Option<syn::Path>,
) -> syn::Result<bool> {
    if !meta.path.is_ident("crate") {
        return Ok(false);
    }
    let lit: syn::LitStr = meta.value()?.parse()?;
    *out = Some(lit.parse()?);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    /// 用 `parse_nested_meta` 跑一遍 `parse_crate_arg`，返回它自己的结果和是否命中 `crate` 这个键
    fn run(attr: &syn::Attribute) -> syn::Result<(bool, Option<syn::Path>)> {
        let mut out = None;
        let mut matched = false;
        attr.parse_nested_meta(|meta| {
            matched = parse_crate_arg(&meta, &mut out)?;
            Ok(())
        })?;
        Ok((matched, out))
    }

    #[test]
    fn parse_crate_arg_valid() {
        let attr: syn::Attribute = syn::parse_quote!(#[hy_err(crate = "::hygiea_core")]);
        let (matched, out) = run(&attr).unwrap();
        assert!(matched);
        let path = out.expect("crate = 命中时应写入 out");
        assert_eq!(quote!(#path).to_string(), quote!(::hygiea_core).to_string());
    }

    #[test]
    fn parse_crate_arg_value_not_string() {
        // crate 的值不是字符串字面量
        let attr: syn::Attribute = syn::parse_quote!(#[hy_err(crate = 123)]);
        assert!(run(&attr).is_err());
    }

    #[test]
    fn parse_crate_arg_not_crate_key() {
        // 不是 `crate` 这个键，直接返回 Ok(false)，不写 out；
        // 写成不带值的裸标识符，避免 parse_nested_meta 因为没消费掉 `= 值` 部分而报另一种错误
        let attr: syn::Attribute = syn::parse_quote!(#[hy_err(other)]);
        let (matched, out) = run(&attr).unwrap();
        assert!(!matched);
        assert!(out.is_none());
    }
}
