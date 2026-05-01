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

pub fn format_template_once(source: &str, args: serde_json::Value) -> Result<String, Error> {
    TEMPLATES.read().env.render_str(source, args)
}

pub fn format_template_cached(source: &str, args: serde_json::Value) -> Result<String, Error> {
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
pub fn format_positional(
    source: &str,
    args: serde_json::Value,
    cached: bool,
) -> Result<String, Error> {
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
    let arr = match args.as_array() {
        Some(a) => a,
        None => {
            return Err(Error::new(
                minijinja::ErrorKind::BadSerialization,
                "positional args must be an array",
            ));
        }
    };
    if arr.len() < count {
        return Err(Error::new(
            minijinja::ErrorKind::MissingArgument,
            format!("not enough arguments: expected {count}, got {}", arr.len()),
        ));
    } else if arr.len() > count {
        return Err(Error::new(
            minijinja::ErrorKind::TooManyArguments,
            format!("too many arguments: expected {count}, got {}", arr.len()),
        ));
    }
    let mut map = serde_json::Map::with_capacity(arr.len());
    for (i, val) in arr.iter().enumerate() {
        map.insert(format!("_{}", i), val.clone());
    }
    let named_args = serde_json::Value::Object(map);

    if cached {
        format_template_cached(&tpl, named_args)
    } else {
        format_template_once(&tpl, named_args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== format_template_once / format_template_cached =====

    #[test]
    fn test_template_once_basic() {
        let result =
            format_template_once("Hello {{ name }}", serde_json::json!({"name": "Alice"})).unwrap();
        assert_eq!(result, "Hello Alice");
    }

    #[test]
    fn test_template_cached_basic() {
        let result =
            format_template_cached("Hello {{ name }}", serde_json::json!({"name": "Bob"})).unwrap();
        assert_eq!(result, "Hello Bob");
    }

    #[test]
    fn test_template_cached_hits_cache() {
        let tpl = "value is {{ v }}";
        let r1 = format_template_cached(tpl, serde_json::json!({"v": 1})).unwrap();
        let r2 = format_template_cached(tpl, serde_json::json!({"v": 2})).unwrap();
        assert_eq!(r1, "value is 1");
        assert_eq!(r2, "value is 2");
    }

    #[test]
    fn test_template_undefined_strict() {
        let result = format_template_once("{{ missing }}", serde_json::json!({}));
        assert!(result.is_err());
    }

    // ===== format_positional =====

    #[test]
    fn test_positional_basic() {
        let result = format_positional("Hello {}", serde_json::json!(["Alice"]), false).unwrap();
        assert_eq!(result, "Hello Alice");
    }

    #[test]
    fn test_positional_multiple_args() {
        let result =
            format_positional("{} has {} messages", serde_json::json!(["Alice", 3]), false)
                .unwrap();
        assert_eq!(result, "Alice has 3 messages");
    }

    #[test]
    fn test_positional_cached() {
        let tpl = "Hi {}";
        let r1 = format_positional(tpl, serde_json::json!(["A"]), true).unwrap();
        let r2 = format_positional(tpl, serde_json::json!(["B"]), true).unwrap();
        assert_eq!(r1, "Hi A");
        assert_eq!(r2, "Hi B");
    }

    #[test]
    fn test_positional_escape_braces() {
        let result =
            format_positional("score: {}%, literal: {{}}", serde_json::json!([95]), false).unwrap();
        assert_eq!(result, "score: 95%, literal: {}");
    }

    #[test]
    fn test_positional_no_placeholder() {
        let result = format_positional("no placeholders", serde_json::json!([]), false).unwrap();
        assert_eq!(result, "no placeholders");
    }

    #[test]
    fn test_positional_not_enough_args() {
        let result = format_positional("{} and {}", serde_json::json!(["only_one"]), false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), minijinja::ErrorKind::MissingArgument);
    }

    #[test]
    fn test_positional_too_many_args() {
        let result = format_positional("{}", serde_json::json!(["a", "b"]), false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), minijinja::ErrorKind::TooManyArguments);
    }

    #[test]
    fn test_positional_not_array() {
        let result = format_positional("{}", serde_json::json!("not array"), false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), minijinja::ErrorKind::BadSerialization);
    }
}
