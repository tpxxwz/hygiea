//! 模板渲染（minijinja），带限量缓存；以及 `{}` 位置参数格式化。

use crate::{BaseErr, HyErr, ResultExt, bail, err};
use lru::LruCache;
use minijinja::{Environment, UndefinedBehavior};
use parking_lot::RwLock;
use serde::Serialize;
use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock};

const CACHE_CAPACITY: NonZeroUsize = match NonZeroUsize::new(1024) {
    Some(n) => n,
    None => NonZeroUsize::MIN,
};

/// 缓存里的模板名，每个 env 只放一个模板
const NAME: &str = "t";

fn strict_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env
}

/// `tpl_once` 用：只读，`render_str` 不需要锁
static ONCE_ENV: LazyLock<Environment<'static>> = LazyLock::new(strict_env);

/// key 是模板原文。每个模板单独一个 env，用 `Arc` 取出来后在锁外渲染，锁里只做查找和插入。
///
/// 用 LruCache 只是为了限制条数：命中用 `peek`，只拿读锁、不调整顺序，所以满了淘汰的是
/// 最早插入的那条（FIFO），不是最久没用的
static CACHE: LazyLock<RwLock<LruCache<String, Arc<Environment<'static>>>>> =
    LazyLock::new(|| RwLock::new(LruCache::new(CACHE_CAPACITY)));

/// 渲染模板，不缓存，每次重新解析。语法错误、变量缺失（Strict 模式）返回 `TemplateError`，
/// minijinja 的原始错误挂在 source 上
pub fn tpl_once(source: &str, args: impl Serialize) -> Result<String, HyErr> {
    ONCE_ENV.render_str(source, args).wrap_err(render_failed)
}

/// 渲染模板，解析结果按模板原文缓存复用（上限 1024 条，满了淘汰最早插入的）。错误同 [`tpl_once`]
pub fn tpl_cached(source: &str, args: impl Serialize) -> Result<String, HyErr> {
    let env = cached_env(source)?;
    env.get_template(NAME)
        .and_then(|t| t.render(args))
        .wrap_err(render_failed)
}

fn render_failed() -> HyErr {
    err!(BaseErr::TemplateError, "render template failed")
}

fn cached_env(source: &str) -> Result<Arc<Environment<'static>>, HyErr> {
    if let Some(env) = CACHE.read().peek(source) {
        return Ok(env.clone());
    }
    // 解析放在锁外；两个线程同时解析同一个模板，后插入的覆盖前一个，结果一样
    let mut env = strict_env();
    env.add_template_owned(NAME, source.to_owned())
        .wrap_err(render_failed)?;
    let env = Arc::new(env);
    CACHE.write().put(source.to_owned(), env.clone());
    Ok(env)
}

/// 位置参数格式化，规则同 Rust 的 `format!`：`{}` 按顺序填入参数，`{{` / `}}` 是字面量 `{` / `}`，
/// 单独的 `{` 或 `}` 报错。直接做字符串替换，不经过 minijinja，所以 `{%`、`{#` 之类都是普通文本。
///
/// 字符串参数原样填入，其余按 JSON 输出（`null`、`true`、`[1,2]`）
pub fn tpl_pos(source: &str, args: &[serde_json::Value]) -> Result<String, HyErr> {
    let mut out = String::with_capacity(source.len() + args.len() * 8);
    let mut next = 0usize;
    let mut chars = source.chars().peekable();

    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('{', Some('{')) | ('}', Some('}')) => {
                chars.next();
                out.push(c);
            }
            ('{', Some('}')) => {
                chars.next();
                if let Some(arg) = args.get(next) {
                    match arg {
                        serde_json::Value::String(s) => out.push_str(s),
                        other => out.push_str(&other.to_string()),
                    }
                }
                next += 1;
            }
            ('{', _) => bail!(
                BaseErr::TemplateError,
                "unmatched `{` in positional template, use `{{` for a literal `{`"
            ),
            ('}', _) => bail!(
                BaseErr::TemplateError,
                "unmatched `}` in positional template, use `}}` for a literal `}`"
            ),
            _ => out.push(c),
        }
    }

    if next != args.len() {
        bail!(
            BaseErr::TemplateError,
            format!("expected {next} positional arguments, got {}", args.len())
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ===== tpl_once / tpl_cached =====

    #[test]
    fn test_tpl_once_basic() {
        let result = tpl_once("Hello {{ name }}", json!({"name": "Alice"})).unwrap();
        assert_eq!(result, "Hello Alice");
    }

    #[test]
    fn test_tpl_cached_basic() {
        let result = tpl_cached("Hello {{ name }}", json!({"name": "Bob"})).unwrap();
        assert_eq!(result, "Hello Bob");
    }

    #[test]
    fn test_tpl_cached_hits_cache() {
        let tpl = "value is {{ v }}";
        let r1 = tpl_cached(tpl, json!({"v": 1})).unwrap();
        let r2 = tpl_cached(tpl, json!({"v": 2})).unwrap();
        assert_eq!(r1, "value is 1");
        assert_eq!(r2, "value is 2");
    }

    /// 参数可以是任何 Serialize，不必先转成 json
    #[test]
    fn test_tpl_accepts_serialize() {
        #[derive(Serialize)]
        struct Ctx {
            name: &'static str,
        }
        assert_eq!(
            tpl_once("hi {{ name }}", Ctx { name: "a" }).unwrap(),
            "hi a"
        );
        assert_eq!(
            tpl_cached("hi {{ name }}", &Ctx { name: "b" }).unwrap(),
            "hi b"
        );
    }

    #[test]
    fn test_tpl_once_undefined_strict() {
        let err = tpl_once("{{ missing }}", json!({})).unwrap_err();
        assert!(err.is(BaseErr::TemplateError));
        // minijinja 的原始错误挂在 source 上
        let source = std::error::Error::source(&err).unwrap();
        assert_eq!(
            source.downcast_ref::<minijinja::Error>().unwrap().kind(),
            minijinja::ErrorKind::UndefinedError
        );
    }

    #[test]
    fn test_tpl_cached_syntax_error_not_cached() {
        let tpl = "{{ broken";
        assert!(
            tpl_cached(tpl, json!({}))
                .unwrap_err()
                .is(BaseErr::TemplateError)
        );
        assert!(!CACHE.read().contains(tpl));
    }

    /// 条数有上限，超出后最早插入的被淘汰
    #[test]
    fn test_tpl_cached_bounded() {
        for i in 0..CACHE_CAPACITY.get() * 2 {
            let tpl = format!("bounded filler {i} {{{{ v }}}}");
            assert_eq!(
                tpl_cached(&tpl, json!({"v": i})).unwrap(),
                format!("bounded filler {i} {i}")
            );
        }
        let cache = CACHE.read();
        assert!(!cache.contains("bounded filler 0 {{ v }}"));
        assert!(cache.len() <= CACHE_CAPACITY.get());
    }

    #[test]
    fn test_tpl_cached_concurrent() {
        let handles: Vec<_> = (0..8)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..200 {
                        let tpl = format!("conc {} {{{{ v }}}}", i % 50);
                        assert_eq!(
                            tpl_cached(&tpl, json!({"v": t})).unwrap(),
                            format!("conc {} {t}", i % 50)
                        );
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    // ===== tpl_pos =====

    #[test]
    fn test_tpl_pos_basic() {
        let result = tpl_pos("Hello {}", &[json!("Alice")]).unwrap();
        assert_eq!(result, "Hello Alice");
    }

    #[test]
    fn test_tpl_pos_multiple_args() {
        let result = tpl_pos("{} has {} messages", &[json!("Alice"), json!(3)]).unwrap();
        assert_eq!(result, "Alice has 3 messages");
    }

    #[test]
    fn test_tpl_pos_value_kinds() {
        let args = [
            json!(null),
            json!(true),
            json!(1.5),
            json!([1, 2]),
            json!({"a": 1}),
        ];
        let result = tpl_pos("{} {} {} {} {}", &args).unwrap();
        assert_eq!(result, r#"null true 1.5 [1,2] {"a":1}"#);
    }

    #[test]
    fn test_tpl_pos_escape_braces() {
        let result = tpl_pos("score: {}%, literal: {{}}", &[json!(95)]).unwrap();
        assert_eq!(result, "score: 95%, literal: {}");
    }

    /// 和 Rust format! 一样的转义语义，jinja 语法只是普通文本
    #[test]
    fn test_tpl_pos_literals() {
        assert_eq!(tpl_pos("{{{}}}", &[json!("v")]).unwrap(), "{v}");
        assert_eq!(tpl_pos("{{{{ x }}}}", &[]).unwrap(), "{{ x }}");
        assert_eq!(tpl_pos("{{%", &[]).unwrap(), "{%");
        assert_eq!(tpl_pos("a {{# b", &[]).unwrap(), "a {# b");
        // 没转义的 jinja 语法按 format! 规则是单独的 `{`
        let err = tpl_pos("{% if %}", &[]).unwrap_err();
        assert!(err.to_string().contains("unmatched `{`"), "{err}");
    }

    #[test]
    fn test_tpl_pos_unmatched_brace() {
        for tpl in ["{", "a { b", "}", "{x}"] {
            let err = tpl_pos(tpl, &[]).unwrap_err();
            assert!(err.is(BaseErr::TemplateError), "{tpl}");
        }
    }

    #[test]
    fn test_tpl_pos_no_placeholder() {
        let result = tpl_pos("no placeholders", &[]).unwrap();
        assert_eq!(result, "no placeholders");
    }

    #[test]
    fn test_tpl_pos_not_enough_args() {
        let err = tpl_pos("{} and {}", &[json!("only_one")]).unwrap_err();
        assert!(err.is(BaseErr::TemplateError));
        assert!(
            err.to_string()
                .contains("expected 2 positional arguments, got 1")
        );
    }

    #[test]
    fn test_tpl_pos_too_many_args() {
        let err = tpl_pos("{}", &[json!("a"), json!("b")]).unwrap_err();
        assert!(err.is(BaseErr::TemplateError));
        assert!(
            err.to_string()
                .contains("expected 1 positional arguments, got 2")
        );
    }
}
