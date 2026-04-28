use proc_macro::TokenStream;
use quote::{format_ident, quote};
use std::collections::HashMap;
use std::marker::PhantomData;
use syn::spanned::Spanned;

// 真正带 #[proc_macro_derive] 的入口必须写在 lib.rs 里。
// 这里放的是 error 相关宏的具体实现：解析输入 AST、校验属性、生成 Rust 代码。
//
// 过程宏里常见的几个类型：
// - proc_macro::TokenStream：编译器传进来/要求返回的 token 流。
// - syn::DeriveInput：syn 把 derive 输入解析出来的结构化 AST。
// - proc_macro2::TokenStream：quote/syn 生态里更好用的 token 流。
pub(crate) fn derive<V>(input: TokenStream, derive_name: &str) -> TokenStream
where
    V: VariantBuilder,
{
    let ast = syn::parse_macro_input!(input as syn::DeriveInput);
    let ctx = ErrContext::<V>::new();
    expand(ast, derive_name, ctx, V::new).into()
}

// ========== Traits ==========

// 两个 derive 宏的流程基本一样：
// 1. enum 上读 #[err_code_prefix = "..."]。
// 2. 每个 variant 上读 #[error(...)]。
// 3. 校验错误码。
// 4. 生成 impl 和注册信息。
//
// 不同点在“生成什么”：
// - fmt_err：生成 FmtErr，并注册模板 err_tpl。
// - raw_err：生成 RawErr，并注册固定文案 err_msg。
//
// VariantBuilder 表示“一个 enum variant 被解析后的中间状态”。
//
// 刚创建时只有 variant 名字，还不知道 err_code / err_tpl / err_msg。
// parse_error_attr 解析到一个属性项，就调用 update 填进去。
// 全部属性解析完后，validate 负责：
// - 检查必填字段是否存在。
// - 检查 err_code 是否为 5 位数字。
// - 把 enum prefix + variant err_code 拼成最终 8 位错误码。
pub(crate) trait VariantBuilder: Sized {
    fn new(var_name: syn::Ident) -> Self;
    fn update(
        &mut self,
        ident: &str,
        lit_val: String,
        path_span: proc_macro2::Span,
        lit_span: proc_macro2::Span,
    ) -> Result<(), proc_macro2::TokenStream>;
    fn validate(&mut self, err_code_prefix: &str) -> Result<(), proc_macro2::TokenStream>;
    fn err_code(&self) -> &str;
    fn err_code_span(&self) -> proc_macro2::Span;
    fn match_arm(&self, enum_name: &syn::Ident) -> proc_macro2::TokenStream;
    fn generated_items(&self, enum_name: &syn::Ident) -> Vec<proc_macro2::TokenStream>;
    fn build_impl(
        enum_name: &syn::Ident,
        match_arms: &[proc_macro2::TokenStream],
        generated_items: &[proc_macro2::TokenStream],
    ) -> proc_macro2::TokenStream;
}

// ErrContext 是两个宏共用的“收集器”。
// 它不关心自己处理的是 FmtVariant 还是 RawVariant，只保存两类生成片段：
// - match_arms：to_err 方法里的 match 分支。
// - generated_items：错误码保护符号、linkme 注册项等额外 item。
//
// PhantomData<V> 表示这个 context 逻辑上属于某种 Variant 类型，
// 但结构体里不需要真的存一个 V。
struct ErrContext<V> {
    match_arms: Vec<proc_macro2::TokenStream>,
    generated_items: Vec<proc_macro2::TokenStream>,
    _marker: PhantomData<V>,
}

impl<V: VariantBuilder> ErrContext<V> {
    fn new() -> Self {
        ErrContext {
            match_arms: Vec::new(),
            generated_items: Vec::new(),
            _marker: PhantomData,
        }
    }

    fn push(&mut self, enum_name: &syn::Ident, variant: V) {
        self.match_arms.push(variant.match_arm(enum_name));
        self.generated_items
            .extend(variant.generated_items(enum_name));
    }

    fn build(self, enum_name: &syn::Ident) -> proc_macro2::TokenStream {
        V::build_impl(enum_name, &self.match_arms, &self.generated_items)
    }
}

/// 校验 variant 上的 5 位错误码，并和 enum 上的 3 位前缀拼成最终错误码。
///
/// 例如：
/// - enum: `#[err_code_prefix = "001"]`
/// - variant: `#[error(err_code = "00001", ...)]`
/// - 最终错误码：`00100001`
fn validate_err_code(
    raw_code: Option<String>,
    err_code_prefix: &str,
    var_name: &syn::Ident,
    err_code_span: Option<proc_macro2::Span>,
) -> Result<String, proc_macro2::TokenStream> {
    match raw_code {
        Some(code) => {
            if !is_numeric_with_len(&code, 5) {
                return Err(expand_err(
                    err_code_span.unwrap_or_else(|| var_name.span()),
                    "err_code must be exactly 5 digits",
                ));
            }
            Ok(format!("{}{}", err_code_prefix, code))
        }
        None => Err(expand_err_span(var_name, "err_code missing")),
    }
}

// ========== FmtErr Implementation ==========

pub(crate) struct FmtVariant {
    err_code: String,
    err_code_span: Option<proc_macro2::Span>,
    err_tpl: Option<String>,
    var_name: syn::Ident,
}

impl VariantBuilder for FmtVariant {
    fn new(var_name: syn::Ident) -> Self {
        FmtVariant {
            err_code: String::new(),
            err_code_span: None,
            err_tpl: None,
            var_name,
        }
    }

    fn update(
        &mut self,
        ident: &str,
        lit_val: String,
        path_span: proc_macro2::Span,
        lit_span: proc_macro2::Span,
    ) -> Result<(), proc_macro2::TokenStream> {
        // 这里处理 #[error(...)] 里的每个 key-value。
        // fmt_err 只接受 err_code 和 err_tpl。
        match ident {
            "err_code" => {
                self.err_code = lit_val;
                self.err_code_span = Some(lit_span);
            }
            "err_tpl" => self.err_tpl = Some(lit_val),
            _ => {
                return Err(expand_err(
                    path_span,
                    &format!(
                        "unknown attribute `{}`, expected `err_code` or `err_tpl`",
                        ident
                    ),
                ));
            }
        }
        Ok(())
    }

    fn validate(&mut self, err_code_prefix: &str) -> Result<(), proc_macro2::TokenStream> {
        // err_code 先临时存 5 位局部码，校验后改写成 8 位完整码。
        let raw_code = if self.err_code.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.err_code))
        };
        self.err_code = validate_err_code(
            raw_code,
            err_code_prefix,
            &self.var_name,
            self.err_code_span,
        )?;

        if self.err_tpl.is_none() {
            return Err(expand_err_span(&self.var_name, "err_tpl missing"));
        }
        Ok(())
    }

    fn err_code(&self) -> &str {
        &self.err_code
    }

    fn err_code_span(&self) -> proc_macro2::Span {
        self.err_code_span.unwrap_or_else(|| self.var_name.span())
    }

    fn match_arm(&self, enum_name: &syn::Ident) -> proc_macro2::TokenStream {
        let err_code = &self.err_code;
        let err_tpl = self.err_tpl.as_ref().unwrap();
        let var_name = &self.var_name;

        // 生成类似：
        //
        // MyErrors::UserNotFound => ::hygiea::FmtErr {
        //     err_code: "00100001",
        //     err_tpl: "User {{ name }} not found",
        //     err_args: args.clone(),
        // }
        quote! {
            #enum_name::#var_name => ::hygiea::FmtErr {
                err_code: #err_code,
                err_tpl: #err_tpl,
                err_args: args.clone(),
            }
        }
    }

    fn generated_items(&self, enum_name: &syn::Ident) -> Vec<proc_macro2::TokenStream> {
        let err_code = &self.err_code;
        let err_code_span = self.err_code_span();
        let err_tpl = self.err_tpl.as_ref().unwrap();
        let var_name = &self.var_name;

        // 生成重复错误码保护和 linkme 注册项。
        // linkme 注册项会被 hygiea-core 收集到 ERR_REGISTRATIONS。
        vec![
            build_err_code_guard(enum_name, var_name, err_code, err_code_span),
            build_template_registration(enum_name, var_name, err_code, err_tpl),
        ]
    }

    fn build_impl(
        enum_name: &syn::Ident,
        match_arms: &[proc_macro2::TokenStream],
        generated_items: &[proc_macro2::TokenStream],
    ) -> proc_macro2::TokenStream {
        quote! {
            impl #enum_name {
                /// Converts this error variant into a [`FmtErr`] with the given arguments.
                pub fn to_err(&self, args: serde_json::Value) -> ::hygiea::FmtErr {
                    match self {
                        #(#match_arms),*
                    }
                }
            }
            #(#generated_items)*
        }
    }
}

// ========== RawErr Implementation ==========

pub(crate) struct RawVariant {
    err_code: String,
    err_code_span: Option<proc_macro2::Span>,
    err_msg: Option<String>,
    var_name: syn::Ident,
}

impl VariantBuilder for RawVariant {
    fn new(var_name: syn::Ident) -> Self {
        RawVariant {
            err_code: String::new(),
            err_code_span: None,
            err_msg: None,
            var_name,
        }
    }

    fn update(
        &mut self,
        ident: &str,
        lit_val: String,
        path_span: proc_macro2::Span,
        lit_span: proc_macro2::Span,
    ) -> Result<(), proc_macro2::TokenStream> {
        // raw_err 只接受 err_code 和 err_msg。
        match ident {
            "err_code" => {
                self.err_code = lit_val;
                self.err_code_span = Some(lit_span);
            }
            "err_msg" => self.err_msg = Some(lit_val),
            _ => {
                return Err(expand_err(
                    path_span,
                    &format!(
                        "unknown attribute `{}`, expected `err_code` or `err_msg`",
                        ident
                    ),
                ));
            }
        }
        Ok(())
    }

    fn validate(&mut self, err_code_prefix: &str) -> Result<(), proc_macro2::TokenStream> {
        // 和 FmtVariant 一样，把 5 位局部码变成带 prefix 的 8 位完整码。
        let raw_code = if self.err_code.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.err_code))
        };
        self.err_code = validate_err_code(
            raw_code,
            err_code_prefix,
            &self.var_name,
            self.err_code_span,
        )?;

        if self.err_msg.is_none() {
            return Err(expand_err_span(&self.var_name, "err_msg missing"));
        }
        Ok(())
    }

    fn err_code(&self) -> &str {
        &self.err_code
    }

    fn err_code_span(&self) -> proc_macro2::Span {
        self.err_code_span.unwrap_or_else(|| self.var_name.span())
    }

    fn match_arm(&self, enum_name: &syn::Ident) -> proc_macro2::TokenStream {
        let err_code = &self.err_code;
        let err_msg = self.err_msg.as_ref().unwrap();
        let var_name = &self.var_name;

        // 生成类似：
        //
        // MyErrors::DbFailed => ::hygiea::RawErr {
        //     err_code: "00200001",
        //     err_msg: "Database connection failed",
        // }
        quote! {
            #enum_name::#var_name => ::hygiea::RawErr {
                err_code: #err_code,
                err_msg: #err_msg,
            }
        }
    }

    fn generated_items(&self, enum_name: &syn::Ident) -> Vec<proc_macro2::TokenStream> {
        let err_code = &self.err_code;
        let err_code_span = self.err_code_span();
        let err_msg = self.err_msg.as_ref().unwrap();
        let var_name = &self.var_name;

        vec![
            build_err_code_guard(enum_name, var_name, err_code, err_code_span),
            build_raw_registration(enum_name, var_name, err_code, err_msg),
        ]
    }

    fn build_impl(
        enum_name: &syn::Ident,
        match_arms: &[proc_macro2::TokenStream],
        generated_items: &[proc_macro2::TokenStream],
    ) -> proc_macro2::TokenStream {
        quote! {
            impl #enum_name {
                /// Converts this error variant into a [`RawErr`].
                pub fn to_err(&self) -> ::hygiea::RawErr {
                    match self {
                        #(#match_arms),*
                    }
                }
            }
            #(#generated_items)*
        }
    }
}

// ========== Common Functions ==========

fn build_err_code_guard(
    enum_name: &syn::Ident,
    var_name: &syn::Ident,
    err_code: &str,
    span: proc_macro2::Span,
) -> proc_macro2::TokenStream {
    let guard = format_ident!("WJJ_STD_ERR_CODE_{}", err_code, span = span);
    let link_guard = link_guard_ident(enum_name, var_name);
    let symbol = format!("__hygiea_err_code_{}", err_code);
    // 这里做两层重复错误码保护：
    //
    // 1. const 名字里包含 err_code。
    //    如果同一个模块里生成了两次同名 const，Rust 编译阶段会报重复定义。
    //
    // 2. #[unsafe(export_name = "...")] 导出固定符号名。
    //    如果不同模块里用了同一个 err_code，Rust 层面的 const 名字不冲突，
    //    但链接阶段会发现导出符号重复，从而报错。
    quote! {
        #[allow(dead_code)]
        const #guard: () = ();

        #[used]
        #[unsafe(export_name = #symbol)]
        static #link_guard: u8 = 0;
    }
}

/// 构建 raw 错误注册项。
///
/// 生成的 static 会挂到 linkme 的 distributed_slice 上。
/// 最终 hygiea-core 可以通过 ERR_REGISTRATIONS 拿到所有 crate 内注册的错误。
fn build_raw_registration(
    enum_name: &syn::Ident,
    var_name: &syn::Ident,
    err_code: &str,
    err_msg: &str,
) -> proc_macro2::TokenStream {
    let err_reg = registration_ident(enum_name, var_name);
    quote! {
        #[linkme::distributed_slice(::hygiea::__private::ERR_REGISTRATIONS)]
        static #err_reg: ::hygiea::__private::ErrRegistration =
            ::hygiea::__private::ErrRegistration {
                err_code: #err_code,
                kind: ::hygiea::__private::ErrRegistrationKind::Raw {
                    err_msg: #err_msg,
                },
            };
    }
}

/// 构建模板错误注册项。
///
/// FmtErr 的 Display 会用 err_code 从 minijinja Environment 里取模板，
/// 所以这里必须把 err_code 和 err_tpl 注册进去。
fn build_template_registration(
    enum_name: &syn::Ident,
    var_name: &syn::Ident,
    err_code: &str,
    err_tpl: &str,
) -> proc_macro2::TokenStream {
    let err_reg = registration_ident(enum_name, var_name);
    quote! {
        #[linkme::distributed_slice(::hygiea::__private::ERR_REGISTRATIONS)]
        static #err_reg: ::hygiea::__private::ErrRegistration =
            ::hygiea::__private::ErrRegistration {
                err_code: #err_code,
                kind: ::hygiea::__private::ErrRegistrationKind::Template {
                    err_tpl: #err_tpl,
                },
            };
    }
}

fn registration_ident(enum_name: &syn::Ident, var_name: &syn::Ident) -> syn::Ident {
    let enum_name = sanitize_ident_part(&enum_name.to_string());
    let var_name = sanitize_ident_part(&var_name.to_string());
    format_ident!("ERR_REG_{}_{}", enum_name, var_name)
}

fn link_guard_ident(enum_name: &syn::Ident, var_name: &syn::Ident) -> syn::Ident {
    let enum_name = sanitize_ident_part(&enum_name.to_string());
    let var_name = sanitize_ident_part(&var_name.to_string());
    format_ident!("WJJ_STD_ERR_CODE_LINK_GUARD_{}_{}", enum_name, var_name)
}

fn sanitize_ident_part(value: &str) -> String {
    // 生成 Rust 标识符时只能使用合法字符。
    // 这里把 enum/variant 名字转成大写 ASCII，并把非字母数字替换成下划线。
    // trim_start_matches("r#") 是为了兼容 raw identifier，例如 r#type。
    value
        .trim_start_matches("r#")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn expand<V, F>(
    ast: syn::DeriveInput,
    derive_name: &str,
    mut ctx: ErrContext<V>,
    variant_new: F,
) -> proc_macro2::TokenStream
where
    V: VariantBuilder,
    F: Fn(syn::Ident) -> V,
{
    // 两个 derive 宏共用的主流程。
    //
    // 输入示例 1：fmt_err 使用 err_tpl，生成 FmtErr。
    //
    // #[derive(fmt_err)]
    // #[err_code_prefix = "001"]
    // enum UserErr {
    //     #[error(err_code = "00001", err_tpl = "User {{ name }} not found")]
    //     UserNotFound,
    // }
    //
    // 输入示例 2：raw_err 使用 err_msg，生成 RawErr。
    //
    // #[derive(raw_err)]
    // #[err_code_prefix = "002"]
    // enum SystemErr {
    //     #[error(err_code = "00001", err_msg = "Database connection failed")]
    //     DbConnectionFailed,
    // }
    //
    // expand 做的事情：
    // 1. 确认 derive 用在 enum 上。
    // 2. 从 enum 上读取 err_code_prefix。
    // 3. 遍历每个 variant，读取 #[error(...)]。
    // 4. 校验字段、拼接完整 err_code、检查同 enum 内重复码。
    // 5. 调用 ctx.build 生成最终 Rust 代码。
    //
    // fmt_err 和 raw_err 的差异不写在 expand 里，而是由 V: VariantBuilder 决定。
    let enum_name = &ast.ident;
    let syn::Data::Enum(ref e) = ast.data else {
        return expand_err_span(&ast, &format!("{} only works on enums", derive_name));
    };
    let variants = &e.variants;

    // 解析 enum 上的 #[err_code_prefix = "..."]。
    // 同时禁止把 #[error(...)] 写在 enum 上，因为 error 只允许写在 variant 上。
    let mut err_code_prefix: Option<String> = None;
    for attr in &ast.attrs {
        if attr.path().is_ident("error") {
            return expand_err_span(attr, "`error` attribute is only allowed on enum variants");
        }
        if attr.path().is_ident("err_code_prefix") {
            match parse_err_code_prefix(attr) {
                Ok(prefix) => err_code_prefix = Some(prefix),
                Err(e) => return e,
            }
        }
    }

    let err_code_prefix = match err_code_prefix {
        Some(prefix) => prefix,
        None => {
            return expand_err_span(
                &ast.ident,
                "`err_code_prefix` attribute is required on the enum",
            );
        }
    };

    let mut seen_err_codes: HashMap<String, syn::Ident> = HashMap::new();

    // 逐个解析 enum variant。这里的 V 是泛型：
    // - fmt_err 时是 FmtVariant。
    // - raw_err 时是 RawVariant。
    for variant in variants {
        let var_name = &variant.ident;
        let mut v = variant_new(var_name.clone());

        for attr in &variant.attrs {
            if attr.path().is_ident("err_code_prefix") {
                return expand_err_span(
                    attr,
                    "`err_code_prefix` attribute is only allowed on the enum, not on variants",
                );
            }
            if attr.path().is_ident("error")
                && let Err(e) = parse_error_attr(attr, &mut v)
            {
                return e;
            }
        }

        let Err(e) = v.validate(&err_code_prefix) else {
            let err_code = v.err_code().to_string();
            // 这一层只检查“同一个 enum 内”的重复错误码。
            // 跨 enum / 跨模块重复由 build_err_code_guard 和运行时 init 校验兜底。
            if let Some(first_variant) = seen_err_codes.get(&err_code) {
                return expand_err(
                    v.err_code_span(),
                    &format!(
                        "duplicate err_code `{}`; first used by variant `{}`",
                        err_code, first_variant
                    ),
                );
            }
            seen_err_codes.insert(err_code, var_name.clone());
            ctx.push(enum_name, v);
            continue;
        };
        return e;
    }

    ctx.build(enum_name)
}

fn parse_err_code_prefix(attr: &syn::Attribute) -> Result<String, proc_macro2::TokenStream> {
    // 只接受这种写法：
    //
    // #[err_code_prefix = "001"]
    //
    // 在 syn 里它会被解析成 Meta::NameValue。
    let syn::Meta::NameValue(nv) = &attr.meta else {
        return Err(expand_err_span(
            attr,
            "`err_code_prefix` must be written as #[err_code_prefix = \"001\"]",
        ));
    };

    let syn::Expr::Lit(expr_lit) = &nv.value else {
        return Err(expand_err_span(
            &nv.value,
            "`err_code_prefix` must be a string literal like \"001\"",
        ));
    };

    let syn::Lit::Str(lit) = &expr_lit.lit else {
        return Err(expand_err_span(
            &expr_lit.lit,
            "`err_code_prefix` value must be a string literal, e.g. \"001\"",
        ));
    };

    let prefix = lit.value();
    if !is_numeric_with_len(&prefix, 3) {
        return Err(expand_err(
            lit.span(),
            "`err_code_prefix` must be exactly 3 digits, e.g. \"001\"",
        ));
    }
    Ok(prefix)
}

fn parse_error_attr<V: VariantBuilder>(
    attr: &syn::Attribute,
    variant: &mut V,
) -> Result<(), proc_macro2::TokenStream> {
    // 只处理这种列表形式：
    //
    // #[error(err_code = "00001", err_tpl = "...")]
    //
    // 每一项会被解析成 MetaNameValue，然后交给 VariantBuilder::update。
    let syn::Meta::List(list) = attr.meta.clone() else {
        return Ok(());
    };

    let args = list
        .parse_args_with(
            syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated,
        )
        .map_err(|e| e.to_compile_error())?;

    for nv in args {
        let Some(i) = nv.path.get_ident() else {
            return Err(expand_err_span(
                &nv.path,
                "expected identifier like `err_code = \"00001\"`",
            ));
        };
        let ident = i.to_string();

        let syn::Expr::Lit(expr_lit) = &nv.value else {
            return Err(expand_err_span(&nv.value, "value must be literal string"));
        };

        let syn::Lit::Str(lit_str) = &expr_lit.lit else {
            return Err(expand_err_span(
                &expr_lit.lit,
                "value must be string literal",
            ));
        };

        variant.update(&ident, lit_str.value(), nv.path.span(), lit_str.span())?;
    }
    Ok(())
}

/// 校验字符串是否为指定长度的纯数字
fn is_numeric_with_len(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit())
}

/// 结构错误 - 用于 AST 节点类型不符、属性放错位置等
fn expand_err_span(token: impl quote::ToTokens, msg: &str) -> proc_macro2::TokenStream {
    syn::Error::new_spanned(token, msg).to_compile_error()
}

/// 值错误 - 用于字符串格式不对、长度不对等
fn expand_err(span: proc_macro2::Span, msg: &str) -> proc_macro2::TokenStream {
    syn::Error::new(span, msg).to_compile_error()
}
