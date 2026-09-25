//! 模板渲染（minijinja），带 LRU 缓存；以及 `{}` 位置参数格式化。

use crate::{BaseErr, HyErr, ResultExt, bail, err};
use lru::LruCache;
use minijinja::{Environment, Error, UndefinedBehavior};
use parking_lot::RwLock;
use std::num::NonZeroUsize;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

const REGISTERED_TEMPLATE_CAPACITY: usize = 1024;

static TEMPLATE_ID: AtomicU64 = AtomicU64::new(0);

fn next_template_id() -> String {
    format!("__t{}", TEMPLATE_ID.fetch_add(1, Ordering::Relaxed))
}

struct TemplateEngine {
    env: Environment<'static>,
    // key: source string, value: internal name in env
    registered: LruCache<String, String>,
}

static TEMPLATES: LazyLock<RwLock<TemplateEngine>> = LazyLock::new(|| {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);

    RwLock::new(TemplateEngine {
        env,
        registered: LruCache::new(
            NonZeroUsize::new(REGISTERED_TEMPLATE_CAPACITY)
                .expect("registered template capacity must be non-zero"),
        ),
    })
});

/// 渲染模板，不缓存，每次重新解析。语法错误、变量缺失（Strict 模式）返回 `TemplateError`，
/// minijinja 的原始错误挂在 source 上
pub fn tpl_once(source: &str, args: serde_json::Value) -> Result<String, HyErr> {
    TEMPLATES
        .read()
        .env
        .render_str(source, args)
        .wrap_err(|| err!(BaseErr::TemplateError, "render template failed"))
}

/// 渲染模板，解析结果按模板原文缓存复用（LRU，上限 1024 条）。错误同 [`tpl_once`]
pub fn tpl_cached(source: &str, args: serde_json::Value) -> Result<String, HyErr> {
    render_cached(source, args).wrap_err(|| err!(BaseErr::TemplateError, "render template failed"))
}

fn render_cached(source: &str, args: serde_json::Value) -> Result<String, Error> {
    if let Some(result) = try_render_from_cache(source, &args) {
        return result;
    }
    let name = ensure_template_registered(source)?;
    TEMPLATES.read().env.get_template(&name)?.render(args)
}

fn ensure_template_registered(source: &str) -> Result<String, Error> {
    let mut engine = TEMPLATES.write();
    if let Some(name) = engine.registered.get(source) {
        return Ok(name.clone());
    }
    let name = next_template_id();
    engine
        .env
        .add_template_owned(name.clone(), source.to_owned())?;
    if let Some((_, evicted_name)) = engine.registered.push(source.to_owned(), name.clone()) {
        engine.env.remove_template(&evicted_name);
    }
    Ok(name)
}

fn try_render_from_cache(source: &str, args: &serde_json::Value) -> Option<Result<String, Error>> {
    let engine = TEMPLATES.read();
    if let Some(name) = engine.registered.peek(source) {
        return Some(
            engine
                .env
                .get_template(name)
                .and_then(|t| t.render(args.clone())),
        );
    }
    None
}

/// 位置参数格式化。模板中 `{}` 为占位符，`{{` / `}}` 转义为字面量 `{` / `}`。
/// `cached` 为 true 时注册到 env 缓存复用，为 false 时直接渲染。
pub fn tpl_pos(source: &str, args: serde_json::Value, cached: bool) -> Result<String, HyErr> {
    // 将位置参数模板转换为 minijinja 模板：`{}` → `{{ _N }}`，`{{` → `{`，`}}` → `}`
    let mut tpl = String::with_capacity(source.len() + 32);
    let mut count = 0usize;
    let mut chars = source.chars().peekable();

    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('{', Some('{')) => {
                chars.next();
                tpl.push('{');
            }
            ('}', Some('}')) => {
                chars.next();
                tpl.push('}');
            }
            ('{', Some('}')) => {
                chars.next();
                tpl.push_str(&format!("{{{{ _{count} }}}}"));
                count += 1;
            }
            _ => tpl.push(c),
        }
    }

    // 校验参数数量并构建命名参数
    let Some(arr) = args.as_array() else {
        bail!(BaseErr::TemplateError, "positional args must be an array");
    };
    if arr.len() != count {
        bail!(
            BaseErr::TemplateError,
            format!("expected {count} positional arguments, got {}", arr.len())
        );
    }
    let mut map = serde_json::Map::with_capacity(arr.len());
    for (i, val) in arr.iter().enumerate() {
        map.insert(format!("_{}", i), val.clone());
    }
    let named_args = serde_json::Value::Object(map);

    if cached {
        tpl_cached(&tpl, named_args)
    } else {
        tpl_once(&tpl, named_args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== tpl_once / tpl_cached =====

    #[test]
    fn test_tpl_once_basic() {
        let result = tpl_once("Hello {{ name }}", serde_json::json!({"name": "Alice"})).unwrap();
        assert_eq!(result, "Hello Alice");
    }

    #[test]
    fn test_tpl_cached_basic() {
        let result = tpl_cached("Hello {{ name }}", serde_json::json!({"name": "Bob"})).unwrap();
        assert_eq!(result, "Hello Bob");
    }

    #[test]
    fn test_tpl_cached_hits_cache() {
        let tpl = "value is {{ v }}";
        let r1 = tpl_cached(tpl, serde_json::json!({"v": 1})).unwrap();
        let r2 = tpl_cached(tpl, serde_json::json!({"v": 2})).unwrap();
        assert_eq!(r1, "value is 1");
        assert_eq!(r2, "value is 2");
    }

    #[test]
    fn test_tpl_once_undefined_strict() {
        let err = tpl_once("{{ missing }}", serde_json::json!({})).unwrap_err();
        assert!(err.is(BaseErr::TemplateError));
        // minijinja 的原始错误挂在 source 上
        let source = std::error::Error::source(&err).unwrap();
        assert_eq!(
            source.downcast_ref::<Error>().unwrap().kind(),
            minijinja::ErrorKind::UndefinedError
        );
    }

    // ===== tpl_pos =====

    #[test]
    fn test_tpl_pos_basic() {
        let result = tpl_pos("Hello {}", serde_json::json!(["Alice"]), false).unwrap();
        assert_eq!(result, "Hello Alice");
    }

    #[test]
    fn test_tpl_pos_multiple_args() {
        let result = tpl_pos("{} has {} messages", serde_json::json!(["Alice", 3]), false).unwrap();
        assert_eq!(result, "Alice has 3 messages");
    }

    #[test]
    fn test_tpl_pos_cached() {
        let tpl = "Hi {}";
        let r1 = tpl_pos(tpl, serde_json::json!(["A"]), true).unwrap();
        let r2 = tpl_pos(tpl, serde_json::json!(["B"]), true).unwrap();
        assert_eq!(r1, "Hi A");
        assert_eq!(r2, "Hi B");
    }

    #[test]
    fn test_tpl_pos_escape_braces() {
        let result = tpl_pos("score: {}%, literal: {{}}", serde_json::json!([95]), false).unwrap();
        assert_eq!(result, "score: 95%, literal: {}");
    }

    #[test]
    fn test_tpl_pos_no_placeholder() {
        let result = tpl_pos("no placeholders", serde_json::json!([]), false).unwrap();
        assert_eq!(result, "no placeholders");
    }

    #[test]
    fn test_tpl_pos_not_enough_args() {
        let err = tpl_pos("{} and {}", serde_json::json!(["only_one"]), false).unwrap_err();
        assert!(err.is(BaseErr::TemplateError));
        assert!(
            err.to_string()
                .contains("expected 2 positional arguments, got 1")
        );
    }

    #[test]
    fn test_tpl_pos_too_many_args() {
        let err = tpl_pos("{}", serde_json::json!(["a", "b"]), false).unwrap_err();
        assert!(err.is(BaseErr::TemplateError));
        assert!(
            err.to_string()
                .contains("expected 1 positional arguments, got 2")
        );
    }

    #[test]
    fn test_tpl_pos_not_array() {
        let err = tpl_pos("{}", serde_json::json!("not array"), false).unwrap_err();
        assert!(err.is(BaseErr::TemplateError));
    }
}
