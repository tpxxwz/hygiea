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

// derive 宏的流程：
// 1. 从 Cargo.toml 读项目前缀，enum 上读可选的 #[err_code_module_prefix = "..."]。
// 2. 每个 variant 上读 #[error(...)]。
// 3. 校验错误码。
// 4. 生成 impl 和注册信息。
//
// 生成 HyErr，并注册模板 err_tpl。没有变量的模板渲染出来就是原样字符串，
// 所以固定文案的错误也走这里，不再单设 raw_err。
// 生成逻辑抽成 VariantBuilder，以后要加别的 derive 可以复用 expand 主流程。
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
    /// `code_head` 是拼在变体错误码前面的部分（项目前缀 + 可选的模块前缀），
    /// `code_len` 是变体自己的 `err_code` 应有的位数，两者合起来总是 8 位
    fn validate(
        &mut self,
        code_head: &str,
        code_len: usize,
    ) -> Result<(), proc_macro2::TokenStream>;
    fn err_code(&self) -> &str;
    fn err_code_span(&self) -> proc_macro2::Span;
    fn match_arm(&self, enum_name: &syn::Ident, krate: &syn::Path) -> proc_macro2::TokenStream;
    fn code_arm(&self, enum_name: &syn::Ident) -> proc_macro2::TokenStream;
    fn vars_arm(&self, enum_name: &syn::Ident) -> proc_macro2::TokenStream;
    fn generated_items(&self, krate: &syn::Path) -> Vec<proc_macro2::TokenStream>;
    fn build_impl(
        enum_name: &syn::Ident,
        krate: &syn::Path,
        match_arms: &[proc_macro2::TokenStream],
        code_arms: &[proc_macro2::TokenStream],
        vars_arms: &[proc_macro2::TokenStream],
        generated_items: &[proc_macro2::TokenStream],
    ) -> proc_macro2::TokenStream;
}

// ErrContext 是 derive 主流程用的“收集器”。
// 它不关心具体的 VariantBuilder，只保存两类生成片段：
// - match_arms：to_err 方法里的 match 分支。
// - code_arms：ErrKind::err_code 里的 match 分支，直接返回错误码。
// - vars_arms：__hygiea_tpl_vars 里的 match 分支，返回模板需要的变量名。
// - generated_items：错误码保护符号、linkme 注册项等额外 item。
//
// PhantomData<V> 表示这个 context 逻辑上属于某种 Variant 类型，
// 但结构体里不需要真的存一个 V。
struct ErrContext<V> {
    /// 生成代码里引用 hygiea 的路径，见 krate.rs
    krate: syn::Path,
    match_arms: Vec<proc_macro2::TokenStream>,
    code_arms: Vec<proc_macro2::TokenStream>,
    vars_arms: Vec<proc_macro2::TokenStream>,
    generated_items: Vec<proc_macro2::TokenStream>,
    _marker: PhantomData<V>,
}

impl<V: VariantBuilder> ErrContext<V> {
    fn new() -> Self {
        ErrContext {
            krate: syn::parse_quote!(::hygiea),
            match_arms: Vec::new(),
            code_arms: Vec::new(),
            vars_arms: Vec::new(),
            generated_items: Vec::new(),
            _marker: PhantomData,
        }
    }

    fn push(&mut self, enum_name: &syn::Ident, variant: V) {
        self.match_arms
            .push(variant.match_arm(enum_name, &self.krate));
        self.code_arms.push(variant.code_arm(enum_name));
        self.vars_arms.push(variant.vars_arm(enum_name));
        self.generated_items
            .extend(variant.generated_items(&self.krate));
    }

    fn build(self, enum_name: &syn::Ident) -> proc_macro2::TokenStream {
        V::build_impl(
            enum_name,
            &self.krate,
            &self.match_arms,
            &self.code_arms,
            &self.vars_arms,
            &self.generated_items,
        )
    }
}

/// 校验 variant 上的错误码位数，并和 `code_head` 拼成 8 位最终错误码。
///
/// 例如：
/// - 项目前缀 `001`、没有模块前缀：`err_code = "00001"`（5 位） → `00100001`
/// - 项目前缀 `001`、模块前缀 `02`：`err_code = "007"`（3 位） → `00102007`
fn validate_err_code(
    raw_code: Option<String>,
    code_head: &str,
    code_len: usize,
    var_name: &syn::Ident,
    err_code_span: Option<proc_macro2::Span>,
) -> Result<String, proc_macro2::TokenStream> {
    match raw_code {
        Some(code) => {
            if !is_numeric_with_len(&code, code_len) {
                let hint = if code_len == 5 {
                    "err_code must be exactly 5 digits"
                } else {
                    "err_code must be exactly 3 digits when the enum has `err_code_module_prefix`"
                };
                return Err(expand_err(
                    err_code_span.unwrap_or_else(|| var_name.span()),
                    hint,
                ));
            }
            Ok(format!("{code_head}{code}"))
        }
        None => Err(expand_err_span(var_name, "err_code missing")),
    }
}

// ========== HyErr Implementation ==========

pub(crate) struct HyVariant {
    err_code: String,
    err_code_span: Option<proc_macro2::Span>,
    err_tpl: Option<String>,
    err_tpl_span: Option<proc_macro2::Span>,
    /// 模板里需要外部传入的变量名（已排序），validate 时由 minijinja 解析得出
    vars: Vec<String>,
    var_name: syn::Ident,
}

impl VariantBuilder for HyVariant {
    fn new(var_name: syn::Ident) -> Self {
        HyVariant {
            err_code: String::new(),
            err_code_span: None,
            err_tpl: None,
            err_tpl_span: None,
            vars: Vec::new(),
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
        // hy_err 只接受 err_code 和 err_tpl。
        let duplicated = match ident {
            "err_code" => self.err_code_span.is_some(),
            "err_tpl" => self.err_tpl_span.is_some(),
            _ => false,
        };
        if duplicated {
            return Err(expand_err(path_span, &format!("duplicate `{ident}`")));
        }
        match ident {
            "err_code" => {
                self.err_code = lit_val;
                self.err_code_span = Some(lit_span);
            }
            "err_tpl" => {
                self.err_tpl = Some(lit_val);
                self.err_tpl_span = Some(lit_span);
            }
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

    fn validate(
        &mut self,
        code_head: &str,
        code_len: usize,
    ) -> Result<(), proc_macro2::TokenStream> {
        // err_code 先临时存变体自己的局部码，校验后改写成 8 位完整码。
        let raw_code = if self.err_code.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.err_code))
        };
        self.err_code = validate_err_code(
            raw_code,
            code_head,
            code_len,
            &self.var_name,
            self.err_code_span,
        )?;
        // 00000000 留给成功响应（hygiea-core 的 SUCCESS_CODE）
        if self.err_code == "00000000" {
            return Err(expand_err(
                self.err_code_span(),
                "err_code 00000000 is reserved for success",
            ));
        }

        let Some(err_tpl) = &self.err_tpl else {
            return Err(expand_err_span(&self.var_name, "err_tpl missing"));
        };

        // 用 minijinja 自己解析模板：语法错误在编译期报在 err_tpl 上，而不是等启动时 init() 才 panic；
        // undeclared_variables 给出模板用到、但模板内没定义的变量，也就是必须由调用方传入的参数
        let env = minijinja::Environment::new();
        let tmpl = env.template_from_str(err_tpl).map_err(|e| {
            expand_err(
                self.err_tpl_span.unwrap_or_else(|| self.var_name.span()),
                &format!("invalid err_tpl: {e}"),
            )
        })?;
        // undeclared_variables 会把 range、dict 这类内置全局函数也算进去，它们不用调用方传
        let globals: std::collections::HashSet<&str> =
            env.globals().map(|(name, _)| name).collect();
        let mut vars: Vec<String> = tmpl
            .undeclared_variables(false)
            .into_iter()
            .filter(|name| !globals.contains(name.as_str()))
            .collect();
        vars.sort();
        self.vars = vars;
        Ok(())
    }

    fn err_code(&self) -> &str {
        &self.err_code
    }

    fn err_code_span(&self) -> proc_macro2::Span {
        self.err_code_span.unwrap_or_else(|| self.var_name.span())
    }

    fn match_arm(&self, enum_name: &syn::Ident, krate: &syn::Path) -> proc_macro2::TokenStream {
        let err_code = &self.err_code;
        // validate 已确认 err_tpl 存在
        let err_tpl = self.err_tpl.as_deref().unwrap_or_default();
        let var_name = &self.var_name;

        // 参数和模板对不对得上由 err! 在编译期检查；绕过 err! 直接调 to_err / to_err_raw 传错了，
        // 结果只是渲染失败，Display 退回输出未渲染的模板，运行时不 panic

        // 生成类似：
        //
        // MyErrors::UserNotFound => {
        //     ::hygiea::HyErr::new("00100001", "User {{ name }} not found", args.unwrap_or_default())
        // }
        quote! {
            #enum_name::#var_name => {
                #krate::HyErr::new(#err_code, #err_tpl, args.unwrap_or_default())
            }
        }
    }

    fn code_arm(&self, enum_name: &syn::Ident) -> proc_macro2::TokenStream {
        let err_code = &self.err_code;
        let var_name = &self.var_name;
        // MyErrors::UserNotFound => "00100001"
        quote! { #enum_name::#var_name => #err_code }
    }

    fn vars_arm(&self, enum_name: &syn::Ident) -> proc_macro2::TokenStream {
        let vars = &self.vars;
        let var_name = &self.var_name;
        // MyErrors::UserNotFound => &["name"]
        quote! { #enum_name::#var_name => &[#(#vars),*] }
    }

    fn generated_items(&self, krate: &syn::Path) -> Vec<proc_macro2::TokenStream> {
        let err_code = &self.err_code;
        let err_code_span = self.err_code_span();
        // validate 已确认 err_tpl 存在
        let err_tpl = self.err_tpl.as_deref().unwrap_or_default();

        // 生成重复错误码保护和 linkme 注册项。
        // linkme 注册项会被 hygiea-core 收集到 ERR_REGISTRATIONS。
        let guard = build_err_code_guard(err_code, err_code_span);
        let registration = build_template_registration(err_code, err_tpl, krate);
        // 链接保护符号和注册项放进匿名 const 块，名字不会和别的变体撞，
        // 同模块重复码只由 guard 报一次
        vec![quote! {
            #guard
            const _: () = { #registration };
        }]
    }

    fn build_impl(
        enum_name: &syn::Ident,
        krate: &syn::Path,
        match_arms: &[proc_macro2::TokenStream],
        code_arms: &[proc_macro2::TokenStream],
        vars_arms: &[proc_macro2::TokenStream],
        generated_items: &[proc_macro2::TokenStream],
    ) -> proc_macro2::TokenStream {
        quote! {
            impl #enum_name {
                /// err! 宏展开用，外部请用 err!(X)：写法用错时它在编译期报错，直接调这里只能运行时 panic。
                /// 必须 pub，因为宏在调用方的 crate 里展开
                #[doc(hidden)]
                pub fn to_err_raw(&self) -> #krate::HyErr {
                    self.__hygiea_build(None)
                }

                /// err! 宏展开用，外部请用 err!(X, 值) / err!(X, { .. })，理由同 to_err_raw
                #[doc(hidden)]
                pub fn to_err(&self, args: #krate::__private::serde_json::Value) -> #krate::HyErr {
                    self.__hygiea_build(Some(args))
                }

                /// 模板需要的变量名（已排序）。err! 宏在 const 块里调用它做编译期检查，
                /// 单变量简写 err!(X, 值) 也靠它取 key
                #[doc(hidden)]
                pub const fn __hygiea_tpl_vars(&self) -> &'static [&'static str] {
                    match self {
                        #(#vars_arms),*
                    }
                }

                fn __hygiea_build(&self, args: Option<#krate::__private::serde_json::Value>) -> #krate::HyErr {
                    match self {
                        #(#match_arms),*
                    }
                }
            }
            // 给 HyErr::is 用，按变体直接返回错误码，不构造 HyErr
            impl #krate::ErrKind for #enum_name {
                fn err_code(&self) -> &'static str {
                    match self {
                        #(#code_arms),*
                    }
                }
            }
            #(#generated_items)*
        }
    }
}

// ========== Common Functions ==========

/// 同模块重复码的编译期保护：const 名字里带错误码，重复时报 E0428
fn build_err_code_guard(err_code: &str, span: proc_macro2::Span) -> proc_macro2::TokenStream {
    let guard = format_ident!("HYGIEA_ERR_CODE_{}", err_code, span = span);
    quote! {
        #[allow(dead_code)]
        const #guard: () = ();
    }
}

/// 构建模板错误注册项，外加跨模块重复码的链接保护。
///
/// HyErr 的 Display 会用 err_code 从 minijinja Environment 里取模板，
/// 所以这里必须把 err_code 和 err_tpl 注册进去。
///
/// 链接保护：导出固定符号名 `__hygiea_err_code_<code>`，不同模块用了同一个错误码时，
/// Linux 上链接期报重复符号；macOS 的 ld64 只给警告，由 hygiea-core 启动时的查重兜底
fn build_template_registration(
    err_code: &str,
    err_tpl: &str,
    krate: &syn::Path,
) -> proc_macro2::TokenStream {
    let symbol = format!("__hygiea_err_code_{}", err_code);
    quote! {
        #[used]
        #[unsafe(export_name = #symbol)]
        static LINK_GUARD: u8 = 0;

        // 路径都经 ::hygiea::__private 转接，下游只依赖 hygiea 就够了，不用自己再加 linkme
        #[#krate::__private::linkme::distributed_slice(#krate::__private::ERR_REGISTRATIONS)]
        #[linkme(crate = #krate::__private::linkme)]
        static REGISTRATION: #krate::__private::ErrRegistration =
            #krate::__private::ErrRegistration {
                err_code: #err_code,
                err_tpl: #err_tpl,
            };
    }
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
    // derive 主流程。
    //
    // 输入示例：
    //
    // #[derive(hy_err)]
    // #[err_code_module_prefix = "01"]
    // enum UserErr {
    //     #[error(err_code = "001", err_tpl = "User {{ name }} not found")]
    //     UserNotFound,
    //     #[error(err_code = "002", err_tpl = "Database connection failed")]
    //     DbConnectionFailed,
    // }
    //
    // expand 做的事情：
    // 1. 确认 derive 用在 enum 上。
    // 2. 确定项目前缀和模块前缀。
    // 3. 遍历每个 variant，读取 #[error(...)]。
    // 4. 校验字段、拼接完整 err_code、检查同 enum 内重复码。
    // 5. 调用 ctx.build 生成最终 Rust 代码。
    //
    // 生成什么不写在 expand 里，而是由 V: VariantBuilder 决定。
    let enum_name = &ast.ident;
    let syn::Data::Enum(ref e) = ast.data else {
        return expand_err_span(&ast, &format!("{} only works on enums", derive_name));
    };
    let variants = &e.variants;
    if !ast.generics.params.is_empty() {
        return expand_err_span(
            &ast.generics,
            &format!("{} does not support generics", derive_name),
        );
    }

    // 解析 enum 上可选的 #[err_code_module_prefix = "..."]（模块级）。
    // 同时禁止把 #[error(...)] 写在 enum 上，因为 error 只允许写在 variant 上。
    let mut module_prefix: Option<String> = None;
    let mut explicit_crate: Option<syn::Path> = None;
    for attr in &ast.attrs {
        if attr.path().is_ident("error") {
            return expand_err_span(attr, "`error` attribute is only allowed on enum variants");
        }
        // #[hy_err(crate = "path")]：显式指定生成代码里引用 hygiea 的路径
        if attr.path().is_ident("hy_err") {
            let parsed = attr.parse_nested_meta(|meta| {
                if crate::krate::parse_crate_arg(&meta, &mut explicit_crate)? {
                    Ok(())
                } else {
                    Err(meta.error("unknown `hy_err` option, expected `crate = \"path\"`"))
                }
            });
            if let Err(e) = parsed {
                return e.to_compile_error();
            }
        }
        if attr.path().is_ident("err_code_module_prefix") {
            match parse_digits_attr(attr, "err_code_module_prefix", 2) {
                Ok(prefix) => module_prefix = Some(prefix),
                Err(e) => return e,
            }
        }
    }

    // 项目前缀只从 Cargo.toml 读：crate 的 [package.metadata.hygiea]，再往上找 workspace 的
    // [workspace.metadata.hygiea]，键都是 err_code_project_prefix；都没有就是 "000"。
    // 读过的 Cargo.toml 用 include_bytes! 记成编译依赖，改了前缀会触发重新编译
    let (project_prefix, tracked_manifests) = match manifest::resolve_prefix() {
        Ok(found) => found,
        Err(msg) => return expand_err_span(&ast.ident, &msg),
    };
    ctx.krate = crate::krate::resolve(explicit_crate);

    // 总位数固定 8：项目前缀 3 位 +（模块前缀 2 位）+ 变体错误码
    let code_head = format!("{project_prefix}{}", module_prefix.as_deref().unwrap_or(""));
    let code_len = 8 - code_head.len();

    let mut seen_err_codes: HashMap<String, syn::Ident> = HashMap::new();

    // 逐个解析 enum variant，V 目前只有 HyVariant。
    for variant in variants {
        let var_name = &variant.ident;
        if !matches!(variant.fields, syn::Fields::Unit) {
            return expand_err_span(
                &variant.fields,
                &format!("{} only supports unit variants", derive_name),
            );
        }
        let mut v = variant_new(var_name.clone());

        for attr in &variant.attrs {
            if attr.path().is_ident("err_code_module_prefix") {
                return expand_err_span(
                    attr,
                    "`err_code_module_prefix` attribute is only allowed on the enum, not on variants",
                );
            }
            if attr.path().is_ident("error")
                && let Err(e) = parse_error_attr(attr, &mut v)
            {
                return e;
            }
        }

        let Err(e) = v.validate(&code_head, code_len) else {
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

    let tracking = tracked_manifests.iter().map(|path| {
        quote! { const _: &[u8] = include_bytes!(#path); }
    });
    let built = ctx.build(enum_name);
    quote! {
        #built
        #(#tracking)*
    }
}

/// 解析 `#[name = "..."]` 形式的数字属性，校验位数
fn parse_digits_attr(
    attr: &syn::Attribute,
    name: &str,
    len: usize,
) -> Result<String, proc_macro2::TokenStream> {
    let example = "0".repeat(len - 1) + "1";
    let syn::Meta::NameValue(nv) = &attr.meta else {
        return Err(expand_err_span(
            attr,
            &format!("`{name}` must be written as #[{name} = \"{example}\"]"),
        ));
    };
    let syn::Expr::Lit(expr_lit) = &nv.value else {
        return Err(expand_err_span(
            &nv.value,
            &format!("`{name}` must be a string literal like \"{example}\""),
        ));
    };
    let syn::Lit::Str(lit) = &expr_lit.lit else {
        return Err(expand_err_span(
            &expr_lit.lit,
            &format!("`{name}` value must be a string literal, e.g. \"{example}\""),
        ));
    };
    let value = lit.value();
    if !is_numeric_with_len(&value, len) {
        return Err(expand_err(
            lit.span(),
            &format!("`{name}` must be exactly {len} digits, e.g. \"{example}\""),
        ));
    }
    Ok(value)
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

// ========== 从 Cargo.toml 读项目前缀 ==========

mod manifest {
    use std::path::{Path, PathBuf};

    /// 没有任何配置时的项目前缀
    const DEFAULT_PREFIX: &str = "000";

    /// 按 crate → workspace 的顺序找 `err_code_project_prefix`，返回前缀和读过的 Cargo.toml 路径。
    /// 配置了但格式不对时返回错误信息
    pub(super) fn resolve_prefix() -> Result<(String, Vec<String>), String> {
        let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") else {
            return Ok((DEFAULT_PREFIX.to_string(), Vec::new()));
        };
        let crate_manifest = Path::new(&dir).join("Cargo.toml");
        let mut tracked = Vec::new();

        if let Some(table) = read(&crate_manifest, &mut tracked) {
            if let Some(prefix) = lookup(&table, "package", &crate_manifest)? {
                return Ok((prefix, tracked));
            }
            // crate 自己就是 workspace 根
            if table.contains_key("workspace") {
                let prefix = lookup(&table, "workspace", &crate_manifest)?;
                return Ok((prefix.unwrap_or_else(|| DEFAULT_PREFIX.into()), tracked));
            }
        }

        // 往上找第一个带 [workspace] 的 Cargo.toml
        for ancestor in Path::new(&dir).ancestors().skip(1) {
            let manifest = ancestor.join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let Some(table) = read(&manifest, &mut tracked) else {
                continue;
            };
            if table.contains_key("workspace") {
                let prefix = lookup(&table, "workspace", &manifest)?;
                return Ok((prefix.unwrap_or_else(|| DEFAULT_PREFIX.into()), tracked));
            }
        }
        Ok((DEFAULT_PREFIX.to_string(), tracked))
    }

    fn read(path: &PathBuf, tracked: &mut Vec<String>) -> Option<toml::Table> {
        let content = std::fs::read_to_string(path).ok()?;
        tracked.push(path.to_string_lossy().into_owned());
        content.parse::<toml::Table>().ok()
    }

    /// 取 `[<section>.metadata.hygiea] err_code_project_prefix`，没配置返回 `Ok(None)`
    fn lookup(table: &toml::Table, section: &str, path: &Path) -> Result<Option<String>, String> {
        let value = table
            .get(section)
            .and_then(|v| v.get("metadata"))
            .and_then(|v| v.get("hygiea"))
            .and_then(|v| v.get("err_code_project_prefix"));
        let Some(value) = value else {
            return Ok(None);
        };
        match value.as_str() {
            Some(prefix) if prefix.len() == 3 && prefix.bytes().all(|b| b.is_ascii_digit()) => {
                Ok(Some(prefix.to_string()))
            }
            _ => Err(format!(
                "[{section}.metadata.hygiea] err_code_project_prefix in {} must be a 3-digit string, e.g. \"001\"",
                path.display()
            )),
        }
    }
}
