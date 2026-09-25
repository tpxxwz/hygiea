pub use hygiea_core::string::*;

/// 渲染 minijinja 模板，解析结果缓存复用（上限 1024 条，满了淘汰最早插入的）。
///
/// 参数可以直接写 JSON 字面量 `{ "a": 1 }`，也可以传任何 `Serialize` 的表达式（`ctx`、`&ctx.args`）
#[macro_export]
macro_rules! fmt_tpl {
    ($tpl:expr, { $($json:tt)* } $(,)?) => {
        $crate::string::tpl_cached($tpl, $crate::__private::serde_json::json!({ $($json)* }))
    };
    ($tpl:expr, $args:expr $(,)?) => {
        $crate::string::tpl_cached($tpl, $args)
    };
}

/// 渲染 minijinja 模板，不缓存，每次重新解析。参数写法同 [`fmt_tpl!`]
#[macro_export]
macro_rules! fmt_tpl_once {
    ($tpl:expr, { $($json:tt)* } $(,)?) => {
        $crate::string::tpl_once($tpl, $crate::__private::serde_json::json!({ $($json)* }))
    };
    ($tpl:expr, $args:expr $(,)?) => {
        $crate::string::tpl_once($tpl, $args)
    };
}

/// 位置参数格式化，规则同 `format!`：`{}` 按顺序填入，`{{` / `}}` 是字面量括号。
/// 直接做字符串替换，不解析模板，不需要缓存
#[macro_export]
macro_rules! fmt_pos {
    ($tpl:expr $(, $arg:expr)* $(,)?) => {
        $crate::string::tpl_pos($tpl, &[$($crate::__private::serde_json::json!($arg)),*])
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_fmt_tpl() {
        let result = fmt_tpl!("{{ a }} + {{ b }}", {"a": 1, "b": 2}).unwrap();
        assert_eq!(result, "1 + 2");
    }

    #[test]
    fn test_fmt_tpl_cached_reuse() {
        let tpl = "Hi {{ name }}";
        let r1 = fmt_tpl!(tpl, {"name": "A"}).unwrap();
        let r2 = fmt_tpl!(tpl, {"name": "B"}).unwrap();
        assert_eq!(r1, "Hi A");
        assert_eq!(r2, "Hi B");
    }

    #[test]
    fn test_fmt_tpl_once() {
        let result = fmt_tpl_once!("Hello {{ name }}", {"name": "Alice"}).unwrap();
        assert_eq!(result, "Hello Alice");
    }

    #[test]
    fn test_fmt_pos() {
        let result = fmt_pos!("{} + {} = {}", 1, 2, 3).unwrap();
        assert_eq!(result, "1 + 2 = 3");
    }

    #[test]
    fn test_fmt_pos_cached_reuse() {
        let tpl = "Hi {}";
        let r1 = fmt_pos!(tpl, "A").unwrap();
        let r2 = fmt_pos!(tpl, "B").unwrap();
        assert_eq!(r1, "Hi A");
        assert_eq!(r2, "Hi B");
    }

    /// 参数是表达式（字段访问、引用）也能用
    #[test]
    fn test_fmt_tpl_expr_args() {
        struct Ctx {
            args: serde_json::Value,
        }
        let ctx = Ctx {
            args: serde_json::json!({"name": "A"}),
        };
        assert_eq!(fmt_tpl!("Hi {{ name }}", &ctx.args).unwrap(), "Hi A");
        assert_eq!(fmt_tpl_once!("Hi {{ name }}", ctx.args).unwrap(), "Hi A");
    }

    #[test]
    fn test_fmt_pos_escape() {
        let result = fmt_pos!("literal: {{}}",).unwrap();
        assert_eq!(result, "literal: {}");
    }

    #[test]
    fn test_fmt_pos_dynamic_tpl() {
        let tpl = std::env::var("NOT_EXIST_TEST_TPL").unwrap_or_else(|_| "msg: {}".to_string());
        let result = fmt_pos!(&tpl, "hello").unwrap();
        assert_eq!(result, "msg: hello");
    }
}
