//! 模板（`hy_err` 的 `err_tpl`、`hy_lock_key` 的 `tpl`）的编译期检查，两个 derive 共用

use crate::error::expand_err;

/// 用 minijinja 解析模板：语法错误在编译期报在模板上，而不是等启动时才发现；
/// 返回模板用到、但模板内没定义的变量（已排序），也就是必须由调用方传入的参数。
/// `label` 是报错里模板属性的名字
pub(crate) fn template_vars(
    tpl: &str,
    span: proc_macro2::Span,
    label: &str,
) -> Result<Vec<String>, proc_macro2::TokenStream> {
    let env = minijinja::Environment::new();
    let tmpl = env
        .template_from_str(tpl)
        .map_err(|e| expand_err(span, &format!("invalid {label}: {e}")))?;
    // undeclared_variables 会把 range、dict 这类内置全局函数也算进去，它们不用调用方传
    let globals: std::collections::HashSet<&str> = env.globals().map(|(name, _)| name).collect();
    let mut vars: Vec<String> = tmpl
        .undeclared_variables(false)
        .into_iter()
        .filter(|name| !globals.contains(name.as_str()))
        .collect();
    vars.sort();
    Ok(vars)
}
