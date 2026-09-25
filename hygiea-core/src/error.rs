use linkme::distributed_slice;
use minijinja::{Environment, UndefinedBehavior};
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::OnceLock;

static ERROR_TEMPLATES: OnceLock<Environment<'static>> = OnceLock::new();

#[doc(hidden)]
pub struct ErrRegistration {
    pub err_code: &'static str,
    pub err_tpl: &'static str,
}

/// 成功响应的 code。不是错误，所以不走 `hy_err`；错误码不能占用它，宏在编译期拦截
pub const SUCCESS_CODE: &str = "00000000";

#[doc(hidden)]
#[distributed_slice]
pub static ERR_REGISTRATIONS: [ErrRegistration] = [..];

/// 进程启动时（core 的 ctor）调用：构建模板，同时发现跨 crate 的重复错误码。
///
/// ctor 里 panic 不能 unwind，会直接 abort 并打一大段栈，所以这里打一行清楚的报错后 `exit(1)`。
/// 下游在自己的 ctor 里先用到 HyErr 时，[`templates`] 已经构建过，这里的 `set` 什么都不做
pub(crate) fn init() {
    match build_templates(ERR_REGISTRATIONS) {
        Ok(env) => {
            let _ = ERROR_TEMPLATES.set(env);
        }
        Err(msg) => {
            eprintln!("hygiea: {msg}");
            std::process::exit(1);
        }
    }
}

/// 同一个错误码只能注册一次。同 crate 内的重复宏在编译期就拦住了，这里兜底跨 crate 的
/// （Linux 上链接期会报重复符号，macOS 的 ld64 只给警告）
fn check_codes<'a>(codes: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = HashSet::new();
    for code in codes {
        if !seen.insert(code) {
            return Err(format!("duplicate err_code `{code}`"));
        }
    }
    Ok(())
}

fn build_templates<'a>(
    regs: impl IntoIterator<Item = &'a ErrRegistration>,
) -> Result<Environment<'static>, String> {
    let regs: Vec<_> = regs.into_iter().collect();
    check_codes(regs.iter().map(|r| r.err_code))?;
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    for reg in regs {
        // 模板语法宏在编译期已经校验过，这里失败只可能是宏和 minijinja 版本不一致
        env.add_template(reg.err_code, reg.err_tpl)
            .map_err(|e| format!("err_code `{}`: invalid err_tpl: {e}", reg.err_code))?;
    }
    Ok(env)
}

/// 一般在启动时由 [`init`] 构建好；下游在自己的 ctor 里比 core 先格式化 HyErr 时，这里现场构建。
/// 现场构建失败（重复码）不 panic：打一行报错、返回空的 env，Display 退回输出未渲染的模板，
/// 随后 core 的 ctor 会发现同一个问题并退出进程
fn templates() -> &'static Environment<'static> {
    ERROR_TEMPLATES.get_or_init(|| {
        build_templates(ERR_REGISTRATIONS).unwrap_or_else(|msg| {
            eprintln!("hygiea: {msg}");
            Environment::new()
        })
    })
}

/// 被包裹的外部错误。`Send + Sync + 'static` 是 `anyhow` / tokio 任务边界的通行约束
type Source = Box<dyn Error + Send + Sync + 'static>;

/// 错误本体，由 `#[derive(hy_err)]` 的 enum 生成：没有模板变量的用 `err!(X)`，
/// 有变量的用 `err!(X, { .. })`，用错了编译期报错。没有变量的模板渲染出来就是原样字符串，固定文案也走这里。
///
/// `Display` 只输出模板渲染结果，是给调用方/客户端看的那句话；`{:#}` 额外把 [`Error::source`] 链
/// 逐层拼在后面（`msg: cause1: cause2`），用于服务端日志，内部细节不会随 `Display` 外泄。
///
/// 外部错误用 [`HyErr::with_source`] 挂上，别再 `to_string()` 塞进模板参数：
/// 那样既丢了原始类型（调用方没法 downcast 判断），又会把内部细节渲染进对外消息
///
/// 字段只读：错误码是 [`HyErr::is`] 的依据，改了会让判断和 Display 对不上
#[derive(Debug)]
pub struct HyErr {
    err_code: &'static str,
    err_tpl: &'static str,
    err_args: serde_json::Value,
    source: Option<Source>,
}

impl HyErr {
    /// 宏生成代码用，`source` 私有所以不能走结构体字面量
    #[doc(hidden)]
    pub fn new(err_code: &'static str, err_tpl: &'static str, err_args: serde_json::Value) -> Self {
        Self {
            err_code,
            err_tpl,
            err_args,
            source: None,
        }
    }

    /// 8 位错误码
    pub fn err_code(&self) -> &'static str {
        self.err_code
    }

    /// 未渲染的模板
    pub fn err_tpl(&self) -> &'static str {
        self.err_tpl
    }

    /// 模板参数，没有变量的模板是 `Null`
    pub fn err_args(&self) -> &serde_json::Value {
        &self.err_args
    }

    /// 挂上导致这个错误的外部错误，重复调用以最后一次为准
    pub fn with_source(mut self, source: impl Into<Source>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// 是不是某个变体：`err.is(BaseErr::JsonError)`。按错误码比较，错误码全局唯一（启动时校验）
    pub fn is(&self, kind: impl ErrKind) -> bool {
        self.err_code == kind.err_code()
    }
}

/// 错误 enum 的变体，由 `#[derive(hy_err)]` 自动实现，给 [`HyErr::is`] 用
pub trait ErrKind {
    fn err_code(&self) -> &'static str;
}

impl fmt::Display for HyErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered = templates()
            .get_template(self.err_code)
            .and_then(|template| template.render(&self.err_args));
        match rendered {
            Ok(output) => f.write_str(&output)?,
            // 渲染失败（key 拼错、缺字段）时输出未渲染的模板：模板是写死的文案，不敏感；
            // args 里可能有 body 之类的内部内容，和渲染错误一起只在 {:#}（服务端日志）里输出
            Err(e) => {
                f.write_str(self.err_tpl)?;
                if f.alternate() {
                    write!(f, " [render failed: {e}; err_args: {}]", self.err_args)?;
                }
            }
        }
        // {:#} 时把 source 链逐层拼在后面
        if f.alternate() {
            let mut source = self.source();
            while let Some(inner) = source {
                write!(f, ": {inner}")?;
                source = inner.source();
            }
        }
        Ok(())
    }
}

/// 接入标准库的错误链：`with_source` 挂上的外部错误通过 `source()` 暴露出去。
///
/// 有了它，`{:#}`、anyhow、tracing 等都能沿着 `source()` 一层层往下找原因，
/// 调用方也能 `err.source().and_then(|e| e.downcast_ref::<reqwest::Error>())` 拿回原始错误。
/// `map(|e| e as _)` 是把 `&(dyn Error + Send + Sync)` 转成签名要求的 `&dyn Error`，只是去掉两个标记
impl Error for HyErr {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|e| e as _)
    }
}

/// 给 `Result` 加包裹外部错误的简写，类似 anyhow 的 `context`：
///
/// ```ignore
/// serde_json::to_string(&v).wrap_err(|| err!(BaseErr::JsonError, "serialize failed"))?;
/// ```
///
/// 闭包只在出错时才调用，成功路径不构造错误
pub trait ResultExt<T> {
    fn wrap_err(self, err: impl FnOnce() -> HyErr) -> Result<T, HyErr>;
}

impl<T, E: Into<Source>> ResultExt<T> for Result<T, E> {
    fn wrap_err(self, err: impl FnOnce() -> HyErr) -> Result<T, HyErr> {
        self.map_err(|e| err().with_source(e))
    }
}

// ---- base errors ------------------------------------------------------------

pub use hygiea_macros::hy_err;

#[doc(hidden)]
pub mod __private {
    pub use super::{ERR_REGISTRATIONS, ErrRegistration};
    // err! 和 #[derive(hy_err)] 展开后要用，调用方不必自己依赖 serde_json / linkme
    pub use linkme;
    pub use serde_json;

    /// `items` 里的每一项都在 `set` 里。err! 在 const 块里用它确认模板变量都给了
    pub const fn all_in(items: &[&str], set: &[&str]) -> bool {
        let mut i = 0;
        while i < items.len() {
            let mut found = false;
            let mut j = 0;
            while j < set.len() {
                if str_eq(items[i], set[j]) {
                    found = true;
                }
                j += 1;
            }
            if !found {
                return false;
            }
            i += 1;
        }
        true
    }

    const fn str_eq(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }
}

/// 构造 [`HyErr`] 的简写，三种写法：
///
/// ```ignore
/// err!(BaseErr::SysErr)                                  // 模板没有变量
/// err!(BaseErr::DateError, "parse failed")               // 模板正好一个变量，直接给值，key 从模板里取
/// err!(BaseHttpErr::RequestFailed, { "method": m, "url": u })    // 显式写出每个 key
/// ```
///
/// 写法和模板对不上（没变量却传了参数、有变量却没传、多变量却用了单值简写）在编译期报错。
/// `{ .. }` 写法里 key 全是字符串字面量时，模板变量漏写、拼错也在编译期报错；多写的 key 允许，
/// 模板不渲染它，但会留在 [`HyErr::err_args`] 里给调用方用（比如 http 错误带上响应 body）。
/// key 用了表达式（`(k): v`）就不检查，交给 minijinja 渲染时判断。值可以是任何 `Serialize` 类型。
///
/// 这些检查在 const 块里做，所以第一个参数必须是常量表达式（`BaseErr::SysErr` 这种路径）：
/// `let k = BaseErr::SysErr; err!(k)` 会报 E0435。
#[macro_export]
macro_rules! err {
    // const 块在编译期求值：变体和调用方式对不上就编译失败，而不是运行时 panic
    ($kind:expr $(,)?) => {{
        const {
            assert!(
                $kind.__hygiea_tpl_vars().is_empty(),
                "this error's template has variables, use err!(X, value) or err!(X, {{ .. }})"
            )
        };
        $kind.to_err_raw()
    }};
    ($kind:expr, { $($json:tt)* } $(,)?) => {{
        const {
            assert!(
                !$kind.__hygiea_tpl_vars().is_empty(),
                "this error's template has no variables, use err!(X)"
            )
        };
        $crate::__hygiea_check_keys!($kind; []; $($json)*);
        $kind.to_err($crate::__private::serde_json::json!({ $($json)* }))
    }};
    ($kind:expr, $value:expr $(,)?) => {{
        const {
            assert!(
                $kind.__hygiea_tpl_vars().len() == 1,
                "err!(X, value) needs a template with exactly one variable, use err!(X) or err!(X, {{ .. }})"
            )
        };
        let mut args = $crate::__private::serde_json::Map::new();
        args.insert(
            $kind.__hygiea_tpl_vars()[0].to_owned(),
            $crate::__private::serde_json::json!($value),
        );
        $kind.to_err($crate::__private::serde_json::Value::Object(args))
    }};
}

/// err! 的内部实现：逐个收集 `{ .. }` 里的字面量 key，最后在 const 块里确认模板变量都给了。
/// 值部分逐个 token 跳过直到下一个逗号（嵌套的 `{}` `[]` `()` 算一个 token，里面的逗号不影响）
#[doc(hidden)]
#[macro_export]
macro_rules! __hygiea_check_keys {
    // @skip 分支要放在前面：否则 `@skip` 会被当成 `$kind:expr` 去解析，直接报错
    (@skip $kind:expr; [$($k:literal)*]; , $($rest:tt)*) => {
        $crate::__hygiea_check_keys!($kind; [$($k)*]; $($rest)*)
    };
    (@skip $kind:expr; [$($k:literal)*]; ) => {
        $crate::__hygiea_check_keys!($kind; [$($k)*]; )
    };
    (@skip $kind:expr; [$($k:literal)*]; $_t:tt $($rest:tt)*) => {
        $crate::__hygiea_check_keys!(@skip $kind; [$($k)*]; $($rest)*)
    };
    ($kind:expr; [$($k:literal)*]; ) => {
        const {
            assert!(
                $crate::__private::all_in($kind.__hygiea_tpl_vars(), &[$($k),*]),
                "err!: a template variable of this error is missing"
            );
        }
    };
    ($kind:expr; [$($k:literal)*]; $key:literal : $($rest:tt)*) => {
        $crate::__hygiea_check_keys!(@skip $kind; [$($k)* $key]; $($rest)*)
    };
    // key 不是字面量：放弃检查
    ($kind:expr; [$($k:literal)*]; $($rest:tt)*) => {};
}

/// 构造错误并立即返回，参数同 [`err!`]。等价于 `return Err(err!(..).into())`，
/// `.into()` 让返回类型是 `anyhow::Error`、`AxumHttpError` 这类时也能直接用，和 `?` 的转换一致。
///
/// 适合函数中途校验不过就提前返回的场景：
///
/// ```ignore
/// fn withdraw(amount: i64, balance: i64) -> Result<(), HyErr> {
///     if amount <= 0 {
///         bail!(BizErr::InvalidAmount, { "amount": amount });   // 带模板参数
///     }
///     if balance == 0 {
///         bail!(BizErr::AccountEmpty);                          // 不带参数
///     }
///     Ok(())
/// }
///
/// // 返回类型是 anyhow::Result 也能直接用，HyErr 会自动转过去
/// fn check(n: i64) -> anyhow::Result<()> {
///     if n < 0 {
///         bail!(BaseErr::SysErr);
///     }
///     Ok(())
/// }
/// ```
#[macro_export]
macro_rules! bail {
    ($($t:tt)*) => {
        return ::core::result::Result::Err(::core::convert::From::from($crate::err!($($t)*)))
    };
}

/// 框架内置错误：默认启用的模块（错误处理、datetime、string、redact 等）用到的都在这里。
/// 需要开 feature 的模块各有自己的错误 enum，比如 http 的 `hygiea::net::http::BaseHttpErr`。
/// 它们共用项目前缀 999（配在 hygiea-core 的 Cargo.toml）。`BaseErr` 不带模块前缀，5 位业务码随意分配，
/// 兜底的 `SysErr` 是 99999；`BaseHttpErr` 用模块前缀 01。是否撞码由 `init()` 的全局查重保证
#[derive(hy_err)]
pub enum BaseErr {
    /// Generic system error. 对外只说 "System Error"，内部原因用 `with_source` 挂上
    #[error(err_code = "99999", err_tpl = "System Error")]
    SysErr,
    /// Date error
    #[error(err_code = "00001", err_tpl = "Date error: {{ cause }}")]
    DateError,
    /// Regex error
    #[error(err_code = "00002", err_tpl = "Invalid regex: {{ pattern }}")]
    RegexError,
    /// JSON serialize / deserialize error
    #[error(err_code = "00003", err_tpl = "JSON error: {{ cause }}")]
    JsonError,
    /// Template render / positional format error
    #[error(err_code = "00004", err_tpl = "Template error: {{ cause }}")]
    TemplateError,
    /// Environment variable not set (and no default)
    #[error(
        err_code = "00005",
        err_tpl = "Environment variable not set: {{ name }}"
    )]
    EnvError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(hy_err)]
    #[hy_err(crate = "crate")]
    enum TestErr {
        #[error(err_code = "90001", err_tpl = "user {{ name }} not found")]
        NotFound,
        #[error(err_code = "90002", err_tpl = "wrapped")]
        Wrapped,
    }

    #[derive(Debug)]
    struct Inner;
    impl fmt::Display for Inner {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("inner cause")
        }
    }
    impl Error for Inner {}

    #[derive(Debug)]
    struct Outer(Inner);
    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("outer")
        }
    }
    impl Error for Outer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    /// Display 只给渲染结果，{:#} 把 source 链逐层拼上
    #[test]
    fn alternate_appends_source_chain() {
        let e = err!(TestErr::Wrapped).with_source(Outer(Inner));
        assert_eq!(e.to_string(), "wrapped");
        assert_eq!(format!("{e:#}"), "wrapped: outer: inner cause");
    }

    /// 渲染失败时 Display 给未渲染的模板、不带 args，细节只在 {:#} 里
    #[test]
    fn render_failure_hides_args() {
        let e = HyErr::new(
            TestErr::NotFound.err_code(),
            "user {{ name }} not found",
            serde_json::json!({ "nmae": "secret-body" }),
        );
        assert_eq!(e.to_string(), "user {{ name }} not found");
        let detailed = format!("{e:#}");
        assert!(detailed.contains("render failed"), "{detailed}");
        assert!(detailed.contains("secret-body"), "{detailed}");
    }

    /// 绕过 err! 直接调隐藏方法、参数和模板对不上：不 panic，Display 退回模板原文
    #[test]
    fn hidden_constructors_do_not_panic() {
        assert_eq!(
            TestErr::NotFound.to_err_raw().to_string(),
            "user {{ name }} not found"
        );
        let e = TestErr::Wrapped.to_err(serde_json::json!({ "x": 1 }));
        assert_eq!(e.to_string(), "wrapped");
    }

    #[test]
    fn wrap_err_attaches_source() {
        let r: Result<(), Inner> = Err(Inner);
        let e = r.wrap_err(|| err!(TestErr::Wrapped)).unwrap_err();
        assert!(e.is(TestErr::Wrapped));
        assert_eq!(e.source().unwrap().to_string(), "inner cause");
    }

    /// 按错误码比较，不同 enum 之间互不相等
    #[test]
    fn is_across_enums() {
        let e = err!(TestErr::NotFound, "a");
        assert!(e.is(TestErr::NotFound));
        assert!(!e.is(TestErr::Wrapped));
        assert!(!e.is(BaseErr::SysErr));
        assert!(err!(BaseErr::SysErr).is(BaseErr::SysErr));
    }

    #[test]
    fn duplicate_codes_rejected() {
        assert!(check_codes(["1", "2"]).is_ok());
        let msg = check_codes(["1", "2", "1"]).unwrap_err();
        assert!(msg.contains("`1`"), "{msg}");

        let regs = [
            ErrRegistration {
                err_code: "1",
                err_tpl: "a",
            },
            ErrRegistration {
                err_code: "1",
                err_tpl: "b",
            },
        ];
        assert!(build_templates(&regs).is_err());
    }

    /// 进程里实际注册的错误码没有重复，模板都能加载
    #[test]
    fn registered_templates_load() {
        let env = templates();
        assert!(env.get_template(BaseErr::SysErr.err_code()).is_ok());
        assert!(env.get_template(TestErr::NotFound.err_code()).is_ok());
    }
}
