//! 模板渲染（minijinja），带限量缓存；以及 `{}` 位置参数格式化。
//!
//! minijinja 模板（[`tpl_once`]、[`tpl_cached`]）是严格模式：用到没定义的变量报错，`default(..)` 过滤器、`is defined` 照常能用。
//! 内置一个函数 `var(name, default?)`：按拼出来的名字在这次渲染的上下文里取值，比如 `{{ var("host_" ~ region) }}`；
//! 取不到时有第二个参数就用它，没有就报错。上下文里有叫 `var` 的值时，它会盖住这个函数。
//!
//! 上下文可以是任何 `Serialize`，也可以是 minijinja 的 [`Value`]：里面放实现了 [`Object`](minijinja::value::Object) 的对象时，
//! 模板用到哪个 key 才调它的 `get_value` 取哪个，没用到的不取（`var()` 取值也一样）。`get_value` 算不出来时返回
//! `Value::from(minijinja::Error)`（无效值），用到它的模板带着这个错误失败，报 `TemplateError`。
//! minijinja 整个从 [`crate::string::minijinja`] 再导出。
//!
//! 多线程同时渲染同一个模板没有问题：缓存的锁里只做查找和插入，渲染在锁外，`Object` 要求 `Send + Sync`。

use crate::{BaseErr, HyErr, Result, ResultExt, bail, err};
use lru::LruCache;
use minijinja::value::{Serde, Value};
use minijinja::{Environment, ErrorKind, State, UndefinedBehavior};
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
    env.add_function("var", var);
    env
}

/// 内置函数 `var(name, default?)`：按名字在这次渲染的上下文里取值，取不到时返回 `default`，没给 `default` 就报没定义
fn var(state: &State, name: &str, default: Option<Value>) -> Result<Value, minijinja::Error> {
    if let Some(value) = state.lookup(name).filter(|value| !value.is_undefined()) {
        return Ok(value);
    }
    default.ok_or_else(|| {
        minijinja::Error::new(
            ErrorKind::UndefinedError,
            format!("variable `{name}` is not defined"),
        )
    })
}

/// `tpl_once` 用：只读，`render_str` 不需要锁
static ONCE_ENV: LazyLock<Environment<'static>> = LazyLock::new(strict_env);

/// key 是模板原文。每个模板单独一个 env，用 `Arc` 取出来后在锁外渲染，锁里只做查找和插入。
///
/// 用 LruCache 只是为了限制条数：命中用 `peek`，只拿读锁、不调整顺序，所以满了淘汰的是
/// 最早插入的那条（FIFO），不是最久没用的
static CACHE: LazyLock<RwLock<LruCache<String, Arc<Environment<'static>>>>> =
    LazyLock::new(|| RwLock::new(LruCache::new(CACHE_CAPACITY)));

/// 渲染模板，不缓存，每次重新解析。`args` 可以是任何 `Serialize`，也可以是 minijinja 的 [`Value`]（用到才取值，见模块文档）。
/// 语法错误、变量缺失（Strict 模式）、上下文里的无效值被用到，返回 `TemplateError`，minijinja 的原始错误挂在 source 上
pub fn tpl_once(source: &str, args: impl Serialize) -> Result<String> {
    ONCE_ENV
        .render_str(source, Serde(args))
        .wrap_err(render_failed)
}

/// 渲染模板，解析结果按模板原文缓存复用（上限 1024 条，满了淘汰最早插入的）。错误同 [`tpl_once`]
pub fn tpl_cached(source: &str, args: impl Serialize) -> Result<String> {
    let env = cached_env(source)?;
    env.get_template(NAME)
        .and_then(|t| t.render(Serde(args)))
        .wrap_err(render_failed)
}

fn render_failed() -> HyErr {
    err!(BaseErr::TemplateError, "render template failed")
}

fn cached_env(source: &str) -> Result<Arc<Environment<'static>>> {
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
pub fn tpl_pos(source: &str, args: &[serde_json::Value]) -> Result<String> {
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
    use serial_test::serial;

    // ===== tpl_once / tpl_cached =====
    // 下面几个测试会读写全局的 CACHE，`test_tpl_cached_bounded` 还会把它冲到上限，
    // 并行跑会互相干扰，所以都标 #[serial(tpl_cache)] 串行

    #[test]
    #[serial(tpl_cache)]
    fn test_tpl_cached_basic() {
        let result = tpl_cached("Hello {{ name }}", json!({"name": "Bob"})).unwrap();
        assert_eq!(result, "Hello Bob");
    }

    #[test]
    #[serial(tpl_cache)]
    fn test_tpl_cached_hits_cache() {
        let tpl = "hits-cache test {{ v }}";
        assert!(!CACHE.read().contains(tpl));
        let r1 = tpl_cached(tpl, json!({"v": 1})).unwrap();
        assert_eq!(r1, "hits-cache test 1");
        assert!(CACHE.read().contains(tpl));
        let len_before = CACHE.read().len();
        // 第二次渲染命中缓存，不会再新增条目
        let r2 = tpl_cached(tpl, json!({"v": 2})).unwrap();
        assert_eq!(r2, "hits-cache test 2");
        assert_eq!(CACHE.read().len(), len_before);
    }

    #[test]
    fn test_tpl_once_basic() {
        let result = tpl_once("Hello {{ name }}", json!({"name": "Alice"})).unwrap();
        assert_eq!(result, "Hello Alice");
    }

    /// 参数可以是任何 Serialize，不必先转成 json
    #[test]
    #[serial(tpl_cache)]
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
    #[serial(tpl_cache)]
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
    #[serial(tpl_cache)]
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
    #[serial(tpl_cache)]
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

    // ===== 内置函数 var =====

    /// 按拼出来的名字取上下文里的值
    #[test]
    fn test_var_by_computed_name() {
        let args = json!({"region": "jp", "host_jp": "jp.example.com"});
        let result = tpl_once("{{ var('host_' ~ region) }}", args).unwrap();
        assert_eq!(result, "jp.example.com");
    }

    /// 取不到时用第二个参数；默认值里可以引用别的变量
    #[test]
    fn test_var_default() {
        let args = json!({"region": "us", "fallback": "f.example.com"});
        assert_eq!(
            tpl_once("{{ var('host_' ~ region, 'x') }}", &args).unwrap(),
            "x"
        );
        assert_eq!(
            tpl_once("{{ var('host_' ~ region, fallback) }}", &args).unwrap(),
            "f.example.com"
        );
    }

    /// 取不到又没给默认值：TemplateError，原始错误是 UndefinedError，消息里有这个名字
    #[test]
    fn test_var_undefined() {
        let err = tpl_once("{{ var('host_' ~ region) }}", json!({"region": "us"})).unwrap_err();
        assert!(err.is(BaseErr::TemplateError));
        let source = std::error::Error::source(&err).unwrap();
        let source = source.downcast_ref::<minijinja::Error>().unwrap();
        assert_eq!(source.kind(), ErrorKind::UndefinedError);
        assert!(source.to_string().contains("host_us"), "{source}");
    }

    /// 缓存的模板里也能用
    #[test]
    #[serial(tpl_cache)]
    fn test_var_in_cached() {
        let result = tpl_cached(
            "var cached {{ var('k_' ~ n) }}",
            json!({"n": 1, "k_1": "v"}),
        )
        .unwrap();
        assert_eq!(result, "var cached v");
    }

    // ===== 上下文是 Object：用到才取值 =====

    /// 记下被取过哪些 key；`broken` 返回无效值
    #[derive(Debug, Default)]
    struct Recording {
        asked: parking_lot::Mutex<Vec<String>>,
    }

    impl minijinja::value::Object for Recording {
        fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
            let name = key.as_str()?;
            self.asked.lock().push(name.to_owned());
            match name {
                "a" => Some(Value::from("A")),
                "b" => Some(Value::from("B")),
                "broken" => Some(Value::from(minijinja::Error::new(
                    ErrorKind::InvalidOperation,
                    "boom",
                ))),
                _ => None,
            }
        }
    }

    fn recording() -> (Arc<Recording>, Value) {
        let root = Arc::new(Recording::default());
        let value = Value::from_dyn_object(root.clone());
        (root, value)
    }

    /// 只取模板里用到的 key，没用到的（包括会出错的 broken）不取；var() 取值同样只取那一个。
    /// 调 `var(..)` 前 minijinja 先在上下文里找名字 `var`，所以也会问一次 `var`，返回 `None` 才用内置函数
    #[test]
    #[serial(tpl_cache)]
    fn test_object_context_is_lazy() {
        let (root, ctx) = recording();
        assert_eq!(tpl_once("{{ a }}", &ctx).unwrap(), "A");
        assert_eq!(tpl_cached("lazy {{ var('b') }}", &ctx).unwrap(), "lazy B");
        assert_eq!(*root.asked.lock(), ["a", "var", "b"]);
    }

    /// Object 里没有的 key 是没定义：直接用报错，default() 接得住
    #[test]
    fn test_object_missing_key() {
        let (_, ctx) = recording();
        assert!(
            tpl_once("{{ nope }}", &ctx)
                .unwrap_err()
                .is(BaseErr::TemplateError)
        );
        assert_eq!(tpl_once("{{ nope | default('d') }}", &ctx).unwrap(), "d");
    }

    /// Object 返回的无效值被用到时，它带的错误一路带出来，在 source 链上
    #[test]
    fn test_object_invalid_value_error() {
        let (_, ctx) = recording();
        let err = tpl_once("{{ a }} {{ broken }}", &ctx).unwrap_err();
        assert!(err.is(BaseErr::TemplateError));
        let source = std::error::Error::source(&err).unwrap();
        assert!(format!("{source:#}").contains("boom"), "{source:#}");
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

    /// tpl_once 不写缓存：多次调用同一个模板，缓存里仍然没有
    #[test]
    #[serial(tpl_cache)]
    fn test_tpl_once_no_cache() {
        let tpl = "tpl_once no cache {{ v }}";
        assert!(!CACHE.read().contains(tpl));
        let _ = tpl_once(tpl, json!({"v": 1})).unwrap();
        assert!(!CACHE.read().contains(tpl));
        let _ = tpl_once(tpl, json!({"v": 2})).unwrap();
        assert!(!CACHE.read().contains(tpl));
    }

    /// 空串和 Unicode 输入
    #[test]
    #[serial(tpl_cache)]
    fn test_tpl_empty_and_unicode() {
        // 空模板
        assert_eq!(tpl_once("", json!({})).unwrap(), "");
        assert_eq!(tpl_cached("", json!({})).unwrap(), "");

        // 纯 Unicode 文本（无变量）
        assert_eq!(
            tpl_once("Hello 世界 🌍", json!({})).unwrap(),
            "Hello 世界 🌍"
        );

        // Unicode 作为变量的值（变量名必须是 ASCII，minijinja 的限制）
        assert_eq!(
            tpl_once("{{ name }}", json!({"name": "太郎"})).unwrap(),
            "太郎"
        );

        // 模板文本中包含 Unicode，再加上变量值中的 Unicode
        assert_eq!(
            tpl_once("User: {{ name }}", json!({"name": "Alice"})).unwrap(),
            "User: Alice"
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
