//! 带编号的 derive（`hy_err`、`hy_lock_type`、`hy_lock_key`）共用的部分：enum 上的前缀属性、
//! Cargo.toml 里的项目前缀、生成代码里引用 hygiea 的路径。
//!
//! 编号 = 项目前缀 3 位 + 可选的模块前缀 + 变体自己的编号，总位数固定（错误码 8 位、锁 9 位）：
//! - 模块前缀 2 位：给使用 hygiea 的项目用
//! - 内部模块前缀 3 位：只给 hygiea 自己的 crate 用（项目前缀 999）。两种布局在同一个项目前缀下会撞号
//!   （"10"+"001" 和 "100"+"01" 都是 10001），所以 999 下只能用 3 位的，其他项目只能用 2 位的

use quote::quote;

use crate::error::{expand_err_span, manifest, parse_digits_attr};

/// hygiea 自己的项目前缀，只有它能用内部模块前缀
pub(crate) const INTERNAL_PROJECT_PREFIX: &str = "999";

/// 各 derive 用到的属性名，报错信息里也用它们
pub(crate) struct Names {
    /// derive 的名字，比如 `hy_err`；也是写 `crate = ".."` 的属性名
    pub derive: &'static str,
    /// 变体上的属性，比如 `error`
    pub variant_attr: &'static str,
    /// 2 位的模块前缀，比如 `err_code_module_prefix`
    pub module: &'static str,
    /// 3 位的内部模块前缀，比如 `err_code_internal_module_prefix`
    pub internal: &'static str,
}

/// enum 上的属性解析完的结果
pub(crate) struct Resolved<'a> {
    pub variants: &'a syn::punctuated::Punctuated<syn::Variant, syn::Token![,]>,
    /// 拼在变体编号前面的部分：项目前缀 + 可选的模块前缀
    pub head: String,
    /// 变体编号应有的位数，和 `head` 合起来是总位数
    pub code_len: usize,
    /// 生成代码里引用 hygiea 的路径
    pub krate: syn::Path,
    /// 读过的 Cargo.toml，记成编译依赖：改了前缀会触发重新编译
    tracking: Vec<String>,
}

impl Resolved<'_> {
    /// 把读过的 Cargo.toml 记成编译依赖，放进生成的代码里
    pub(crate) fn tracking(&self) -> proc_macro2::TokenStream {
        let paths = &self.tracking;
        quote! { #(const _: &[u8] = include_bytes!(#paths);)* }
    }
}

/// 解析 enum：确认是不带泛型的 enum，读 enum 上的前缀属性、`#[<derive>(crate = "..")]`，
/// 读 Cargo.toml 的项目前缀，算出变体编号前面的部分和变体编号的位数
pub(crate) fn resolve<'a>(
    ast: &'a syn::DeriveInput,
    names: &Names,
    total_len: usize,
) -> Result<Resolved<'a>, proc_macro2::TokenStream> {
    let syn::Data::Enum(ref e) = ast.data else {
        return Err(expand_err_span(
            ast,
            &format!("{} only works on enums", names.derive),
        ));
    };
    if !ast.generics.params.is_empty() {
        return Err(expand_err_span(
            &ast.generics,
            &format!("{} does not support generics", names.derive),
        ));
    }

    let mut module_prefix: Option<(String, &syn::Attribute)> = None;
    let mut internal_prefix: Option<(String, &syn::Attribute)> = None;
    let mut explicit_crate: Option<syn::Path> = None;
    for attr in &ast.attrs {
        if attr.path().is_ident(names.variant_attr) {
            return Err(expand_err_span(
                attr,
                &format!(
                    "`{}` attribute is only allowed on enum variants",
                    names.variant_attr
                ),
            ));
        }
        // #[<derive>(crate = "path")]：显式指定生成代码里引用 hygiea 的路径
        if attr.path().is_ident(names.derive) {
            let parsed = attr.parse_nested_meta(|meta| {
                if crate::krate::parse_crate_arg(&meta, &mut explicit_crate)? {
                    Ok(())
                } else {
                    Err(meta.error(format!(
                        "unknown `{}` option, expected `crate = \"path\"`",
                        names.derive
                    )))
                }
            });
            if let Err(e) = parsed {
                return Err(e.to_compile_error());
            }
        }
        if attr.path().is_ident(names.module) {
            module_prefix = Some((parse_digits_attr(attr, names.module, 2)?, attr));
        }
        if attr.path().is_ident(names.internal) {
            internal_prefix = Some((parse_digits_attr(attr, names.internal, 3)?, attr));
        }
    }
    if let (Some(_), Some((_, attr))) = (&module_prefix, &internal_prefix) {
        return Err(expand_err_span(
            attr,
            &format!(
                "use only one of `{}` and `{}`",
                names.module, names.internal
            ),
        ));
    }

    // 项目前缀只从 Cargo.toml 读：crate 的 [package.metadata.hygiea]，再往上找 workspace 的
    // [workspace.metadata.hygiea]，键都是 err_code_project_prefix；都没有就是 "000"
    let (project_prefix, tracking) =
        manifest::resolve_prefix().map_err(|msg| expand_err_span(&ast.ident, &msg))?;
    let krate = crate::krate::resolve(explicit_crate);

    match (&module_prefix, &internal_prefix) {
        (Some((_, attr)), None) if project_prefix == INTERNAL_PROJECT_PREFIX => {
            return Err(expand_err_span(
                attr,
                &format!(
                    "project prefix 999 is hygiea's own; use `{}` (3 digits) instead",
                    names.internal
                ),
            ));
        }
        (None, Some((_, attr))) if project_prefix != INTERNAL_PROJECT_PREFIX => {
            return Err(expand_err_span(
                attr,
                &format!(
                    "`{}` is only for hygiea's own crates (project prefix 999); use `{}` (2 digits)",
                    names.internal, names.module
                ),
            ));
        }
        _ => {}
    }

    let prefix = module_prefix.or(internal_prefix).map(|(p, _)| p);
    let head = format!("{project_prefix}{}", prefix.as_deref().unwrap_or(""));
    Ok(Resolved {
        variants: &e.variants,
        code_len: total_len - head.len(),
        head,
        krate,
        tracking,
    })
}

/// 变体的检查：只能是 unit 变体，模块前缀属性只能写在 enum 上
pub(crate) fn check_variant(
    variant: &syn::Variant,
    names: &Names,
) -> Result<(), proc_macro2::TokenStream> {
    if !matches!(variant.fields, syn::Fields::Unit) {
        return Err(expand_err_span(
            &variant.fields,
            &format!("{} only supports unit variants", names.derive),
        ));
    }
    for attr in &variant.attrs {
        for name in [names.module, names.internal] {
            if attr.path().is_ident(name) {
                return Err(expand_err_span(
                    attr,
                    &format!("`{name}` attribute is only allowed on the enum, not on variants"),
                ));
            }
        }
    }
    Ok(())
}
