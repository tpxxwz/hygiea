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
        return syn::parse_str(&format!("::{name}")).expect("crate name is a valid path");
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
