pub use hygiea_core::format_positional;
pub use hygiea_core::format_template_cached;
pub use hygiea_core::format_template_once;

/// 不注册模板，直接调用 minijinja 的 `env.render_str` 渲染。每次调用都会重新解析模板字符串。
#[macro_export]
macro_rules! fmt_tpl_once {
    ($tpl:expr, $args:tt $(,)?) => {
        $crate::format_template_once($tpl, serde_json::json!($args))
    };
}

/// 将模板字符串注册到 minijinja 的 `env` 中，后续相同模板直接从 LRU 缓存中查找并渲染。
/// LRU 缓存上限为 1024 条，超出后淘汰最早注册的模板。
#[macro_export]
macro_rules! fmt_tpl {
    ($tpl:expr, $args:tt $(,)?) => {
        $crate::format_template_cached($tpl, serde_json::json!($args))
    };
}

/// 位置参数格式化，不注册模板，直接渲染。`{}` 按顺序填入参数，`{{` / `}}` 转义为字面量。
#[macro_export]
macro_rules! fmt_pos_once {
    ($tpl:expr $(, $arg:expr)* $(,)?) => {
        $crate::format_positional($tpl, serde_json::json!([$($arg),*]), false)
    };
}

/// 位置参数格式化，注册到 env 缓存复用。`{}` 按顺序填入参数，`{{` / `}}` 转义为字面量。
#[macro_export]
macro_rules! fmt_pos {
    ($tpl:expr $(, $arg:expr)* $(,)?) => {
        $crate::format_positional($tpl, serde_json::json!([$($arg),*]), true)
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_fmt_tpl_once() {
        let result = fmt_tpl_once!("Hello {{ name }}", {"name": "Alice"}).unwrap();
        assert_eq!(result, "Hello Alice");
    }

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
    fn test_fmt_pos_once() {
        let result = fmt_pos_once!("Hello {}", "Alice").unwrap();
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

    #[test]
    fn test_fmt_pos_escape() {
        let result = fmt_pos_once!("literal: {{}}",).unwrap();
        assert_eq!(result, "literal: {}");
    }

    #[test]
    fn test_fmt_pos_dynamic_tpl() {
        let tpl = std::env::var("NOT_EXIST_TEST_TPL").unwrap_or_else(|_| "msg: {}".to_string());
        let result = fmt_pos_once!(&tpl, "hello").unwrap();
        assert_eq!(result, "msg: hello");
    }
}
