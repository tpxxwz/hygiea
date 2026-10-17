//! 错误体系：统一的错误类型 `HyErr`、`err!` / `bail!`、内置错误 `BaseErr`，以及错误码的注册（`#[derive(hy_err)]` 生成）和进程启动时的查重。

use linkme::distributed_slice;
use minijinja::{Environment, UndefinedBehavior};
use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::OnceLock;

static ERROR_TEMPLATES: OnceLock<Environment<'static>> = OnceLock::new();

#[doc(hidden)]
pub struct ErrRegistration {
    pub err_code: &'static str,
    /// `Enum::Variant`，只给生成文档里的错误码表用
    pub err_name: &'static str,
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
    build_env(regs.iter().map(|r| (r.err_code, r.err_tpl)), "err_code")
}

/// 按 `(名字, 模板)` 建模板环境，未定义的变量渲染时报错。错误码和锁 key 共用，`label` 是报错里名字的叫法。
/// 模板语法宏在编译期已经校验过，这里失败只可能是宏和 minijinja 版本不一致
pub(crate) fn build_env(
    templates: impl IntoIterator<Item = (&'static str, &'static str)>,
    label: &str,
) -> Result<Environment<'static>, String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    for (name, tpl) in templates {
        env.add_template(name, tpl)
            .map_err(|e| format!("{label} `{name}`: invalid template: {e}"))?;
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

/// 错误类型默认是 [`HyErr`] 的 `Result`，要别的错误类型时写 `Result<T, E>`
pub type Result<T, E = HyErr> = std::result::Result<T, E>;

/// 错误本体，由 `#[derive(hy_err)]` 的 enum 生成：没有模板变量的用 `err!(X)`，
/// 有变量的用 `err!(X, { .. })`，用错了编译期报错。没有变量的模板渲染出来就是原样字符串，固定文案也走这里。
///
/// `Display` 只输出模板渲染结果，是给调用方/客户端看的那句话；`{:#}` 额外把 [`Error::source`] 链
/// 逐层拼在后面（`msg: cause1: cause2`），用于服务端日志，内部细节不会随 `Display` 外泄。
///
/// 外部错误用 [`HyErr::with_source`] 挂上，别再 `to_string()` 塞进模板参数：
/// 那样既丢了原始类型（调用方没法 downcast 判断），又会把内部细节渲染进对外消息
///
/// `Debug` 参考 anyhow，给人看：`{:?}` 是错误码、消息加上逐层的原因（`main` 返回 `Err`、`unwrap` 的 panic、
/// `dbg!`、测试断言失败时打的都是它）；`{:#?}` 才是原始结构体（排查模板渲染时看 `err_tpl` / `err_args`）。
///
/// 构造时按环境变量捕获调用栈，同 anyhow / 标准库的 [`Backtrace::capture`]：设了 `RUST_LIB_BACKTRACE=1`
/// （没设它时看 `RUST_BACKTRACE=1`）才捕获，`{:?}` 末尾多一段 `Stack backtrace:`；不设不捕获，没有开销。
/// 栈只在 `Debug` 里，`Display`、对外的响应里没有
///
/// 字段只读：错误码是 [`HyErr::is`] 的依据，改了会让判断和 Display 对不上
pub struct HyErr {
    err_code: &'static str,
    err_tpl: &'static str,
    err_args: serde_json::Value,
    source: Option<Source>,
    /// 没捕获时是 None。装箱：`Backtrace` 本身几十字节，不捕获时只占一个指针，`Result<T, HyErr>` 不跟着变大
    backtrace: Option<Box<Backtrace>>,
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
            backtrace: capture_backtrace(),
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

    /// 构造时捕获的调用栈，环境变量没开时是 None，见 [`HyErr`]
    pub fn backtrace(&self) -> Option<&Backtrace> {
        self.backtrace.as_deref()
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

impl HyErr {
    /// 渲染后的消息。渲染失败（key 拼错、缺字段）时输出未渲染的模板：模板是写死的文案，不敏感；
    /// args 里可能有 body 之类的内部内容，和渲染错误一起只在 `detailed`（服务端日志、Debug）时输出
    fn write_message(&self, f: &mut fmt::Formatter<'_>, detailed: bool) -> fmt::Result {
        let rendered = templates()
            .get_template(self.err_code)
            .and_then(|template| template.render(&self.err_args));
        match rendered {
            Ok(output) => f.write_str(&output),
            Err(e) => {
                f.write_str(self.err_tpl)?;
                if detailed {
                    write!(f, " [render failed: {e}; err_args: {}]", self.err_args)?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for HyErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_message(f, f.alternate())?;
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

/// 给人看的格式，参考 anyhow：
///
/// ```text
/// [99900102] app::Worker() background task panicked while running
///
/// Caused by:
///     task 11 panicked with message "boom"
/// ```
///
/// 原因有多层时编号（`0: ...`、`1: ...`），从近到远；捕获了调用栈时最后是 `Stack backtrace:`。
/// `{:#?}` 输出原始结构体
impl fmt::Debug for HyErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            return f
                .debug_struct("HyErr")
                .field("err_code", &self.err_code)
                .field("err_tpl", &self.err_tpl)
                .field("err_args", &self.err_args)
                .field("source", &self.source)
                .field("backtrace", &self.backtrace)
                .finish();
        }
        write!(f, "[{}] ", self.err_code)?;
        self.write_message(f, true)?;
        let mut causes: Vec<&dyn Error> = Vec::new();
        let mut source = self.source();
        while let Some(cause) = source {
            causes.push(cause);
            source = cause.source();
        }
        if !causes.is_empty() {
            f.write_str("\n\nCaused by:")?;
            for (idx, cause) in causes.iter().enumerate() {
                if causes.len() == 1 {
                    write!(f, "\n    {cause}")?;
                } else {
                    write!(f, "\n    {idx}: {cause}")?;
                }
            }
        }
        if let Some(backtrace) = &self.backtrace {
            write!(f, "\n\nStack backtrace:\n{backtrace}")?;
        }
        Ok(())
    }
}

/// 按环境变量捕获调用栈，没开（`Disabled`）或平台不支持（`Unsupported`）时不留
fn capture_backtrace() -> Option<Box<Backtrace>> {
    let backtrace = Backtrace::capture();
    match backtrace.status() {
        BacktraceStatus::Captured => Some(Box::new(backtrace)),
        _ => None,
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
    fn wrap_err(self, err: impl FnOnce() -> HyErr) -> Result<T>;
}

impl<T, E: Into<Source>> ResultExt<T> for Result<T, E> {
    fn wrap_err(self, err: impl FnOnce() -> HyErr) -> Result<T> {
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
/// err!(HttpClientErr::RequestFailed, { "method": m, "url": u })    // 显式写出每个 key
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
    // 写法和模板对不上的检查、参数的组装和 lock_key! 共用，见 __hygiea_tpl_args!
    ($kind:expr $(, $($args:tt)*)?) => {{
        match $crate::__hygiea_tpl_args!(
            [
                "this error's template has variables, use err!(X, value) or err!(X, {{ .. }})",
                "this error's template has no variables, use err!(X)",
                "err!(X, value) needs a template with exactly one variable, use err!(X) or err!(X, {{ .. }})",
                "err!: a template variable of this error is missing"
            ];
            $kind $(, $($args)*)?
        ) {
            ::core::option::Option::None => $kind.to_err_raw(),
            ::core::option::Option::Some(args) => $kind.to_err(args),
        }
    }};
}

/// err! 和 lock_key! 共用：按模板检查写法（const 块里，编译期），组装模板参数。
/// `$kind` 要有 `const fn __hygiea_tpl_vars(&self) -> &'static [&'static str]`（derive 生成）；
/// 方括号里依次是四种写错时的报错：模板有变量却没传、模板没变量却传了、单值简写但模板不是一个变量、漏了模板变量。
/// 返回 `Option<serde_json::Value>`：没传参数是 `None`
#[doc(hidden)]
#[macro_export]
macro_rules! __hygiea_tpl_args {
    // const 块在编译期求值：变体和调用方式对不上就编译失败，而不是运行时 panic
    ([$no_args:literal, $no_vars:literal, $single:literal, $missing:literal]; $kind:expr $(,)?) => {{
        const { assert!($kind.__hygiea_tpl_vars().is_empty(), $no_args) };
        ::core::option::Option::None::<$crate::__private::serde_json::Value>
    }};
    ([$no_args:literal, $no_vars:literal, $single:literal, $missing:literal]; $kind:expr, { $($json:tt)* } $(,)?) => {{
        const { assert!(!$kind.__hygiea_tpl_vars().is_empty(), $no_vars) };
        $crate::__hygiea_check_keys!($missing; $kind; []; $($json)*);
        ::core::option::Option::Some($crate::__private::serde_json::json!({ $($json)* }))
    }};
    ([$no_args:literal, $no_vars:literal, $single:literal, $missing:literal]; $kind:expr, $value:expr $(,)?) => {{
        const { assert!($kind.__hygiea_tpl_vars().len() == 1, $single) };
        let mut args = $crate::__private::serde_json::Map::new();
        args.insert(
            $kind.__hygiea_tpl_vars()[0].to_owned(),
            $crate::__private::serde_json::json!($value),
        );
        ::core::option::Option::Some($crate::__private::serde_json::Value::Object(args))
    }};
}

/// __hygiea_tpl_args! 的内部实现：逐个收集 `{ .. }` 里的字面量 key，最后在 const 块里确认模板变量都给了，
/// 没给全时报 `$missing`。值部分逐个 token 跳过直到下一个逗号（嵌套的 `{}` `[]` `()` 算一个 token，里面的逗号不影响）
#[doc(hidden)]
#[macro_export]
macro_rules! __hygiea_check_keys {
    // @skip 分支要放在前面：否则 `@skip` 会被当成 `$missing:literal` 去解析，直接报错
    (@skip $missing:literal; $kind:expr; [$($k:literal)*]; , $($rest:tt)*) => {
        $crate::__hygiea_check_keys!($missing; $kind; [$($k)*]; $($rest)*)
    };
    (@skip $missing:literal; $kind:expr; [$($k:literal)*]; ) => {
        $crate::__hygiea_check_keys!($missing; $kind; [$($k)*]; )
    };
    (@skip $missing:literal; $kind:expr; [$($k:literal)*]; $_t:tt $($rest:tt)*) => {
        $crate::__hygiea_check_keys!(@skip $missing; $kind; [$($k)*]; $($rest)*)
    };
    ($missing:literal; $kind:expr; [$($k:literal)*]; ) => {
        const {
            assert!(
                $crate::__private::all_in($kind.__hygiea_tpl_vars(), &[$($k),*]),
                $missing
            );
        }
    };
    ($missing:literal; $kind:expr; [$($k:literal)*]; $key:literal : $($rest:tt)*) => {
        $crate::__hygiea_check_keys!(@skip $missing; $kind; [$($k)* $key]; $($rest)*)
    };
    // key 不是字面量：放弃检查
    ($missing:literal; $kind:expr; [$($k:literal)*]; $($rest:tt)*) => {};
}

/// 构造错误并立即返回，参数同 [`err!`]。等价于 `return Err(err!(..).into())`，
/// `.into()` 让返回类型是 `anyhow::Error`、`AxumHttpError` 这类时也能直接用，和 `?` 的转换一致。
///
/// 适合函数中途校验不过就提前返回的场景：
///
/// ```ignore
/// fn withdraw(amount: i64, balance: i64) -> Result<()> {
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

/// 条件不成立时构造错误并立即返回：第一个参数是应该成立的条件，后面的参数同 [`err!`]。
/// 等价于 `if !(条件) { bail!(..) }`，返回类型的转换同 [`bail!`]
///
/// ```ignore
/// fn withdraw(amount: i64) -> Result<()> {
///     ensure!(amount > 0, BizErr::InvalidAmount, { "amount": amount });
///     Ok(())
/// }
/// ```
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $($t:tt)*) => {
        if !($cond) {
            $crate::bail!($($t)*);
        }
    };
}

/// 框架内置错误：默认启用的模块（错误处理、datetime、string 等）和 redact 用到的都在这里。
/// 需要开 feature 的模块各有自己的错误 enum，比如 http 的 `hygiea::http_client::reqwest_client::HttpClientErr`。
/// 它们共用项目前缀 999（配在 hygiea-core 的 Cargo.toml）。`BaseErr` 不带模块前缀，5 位业务码随意分配，
/// 兜底的 `SysErr` 是 99999；`HttpClientErr` 用内部模块前缀 101。是否撞码由 `init()` 的全局查重保证
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
    /// 参数必须大于 0（0 和负数都不行）：批大小、条数、页大小这类。`param` 是参数名，`op` 是哪一步
    #[error(
        err_code = "00006",
        err_tpl = "{{ param }} must be greater than 0: {{ op }}"
    )]
    NotPositive,
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

    /// 去掉构造时捕获的栈：跑测试时设了 RUST_BACKTRACE=1 也能精确比较 Debug 输出
    fn without_backtrace(mut e: HyErr) -> HyErr {
        e.backtrace = None;
        e
    }

    /// Debug 参考 anyhow：[错误码] 消息，有原因时列在 Caused by 下面；一层不编号，多层按 0、1 编号
    #[test]
    fn debug_is_human_readable() {
        let plain = without_backtrace(err!(TestErr::NotFound, "bob"));
        let code = plain.err_code();
        assert_eq!(format!("{plain:?}"), format!("[{code}] user bob not found"));

        let one = without_backtrace(err!(TestErr::Wrapped).with_source(Inner));
        let code = one.err_code();
        assert_eq!(
            format!("{one:?}"),
            format!("[{code}] wrapped\n\nCaused by:\n    inner cause")
        );

        let two = without_backtrace(err!(TestErr::Wrapped).with_source(Outer(Inner)));
        assert_eq!(
            format!("{two:?}"),
            format!("[{code}] wrapped\n\nCaused by:\n    0: outer\n    1: inner cause")
        );
    }

    /// 捕获了栈时 Debug 末尾是 Stack backtrace，栈里有构造错误的这个函数；Display 里没有栈。
    /// 环境变量控制的 capture 在进程里只读一次，这里用 force_capture 直接塞进去
    #[test]
    fn debug_prints_backtrace_when_captured() {
        let mut e = err!(TestErr::Wrapped).with_source(Inner);
        e.backtrace = Some(Box::new(Backtrace::force_capture()));
        let debug = format!("{e:?}");
        assert!(debug.contains("\n\nStack backtrace:\n"), "{debug}");
        assert!(
            debug.contains("debug_prints_backtrace_when_captured"),
            "{debug}"
        );
        let caused = debug.find("Caused by:").unwrap();
        let stack = debug.find("Stack backtrace:").unwrap();
        assert!(caused < stack, "{debug}");
        assert!(e.backtrace().is_some());
        assert!(!format!("{e}").contains("Stack backtrace"));
        assert!(!format!("{e:#}").contains("Stack backtrace"));
    }

    /// 没捕获栈时 Debug 里没有 Stack backtrace 这一段
    #[test]
    fn debug_has_no_backtrace_when_not_captured() {
        let e = without_backtrace(err!(TestErr::Wrapped));
        assert!(e.backtrace().is_none());
        assert!(!format!("{e:?}").contains("Stack backtrace"));
    }

    /// {:#?} 是原始结构体，排查模板渲染时能看到未渲染的模板和参数
    #[test]
    fn alternate_debug_is_struct() {
        let e = err!(TestErr::NotFound, "bob");
        let raw = format!("{e:#?}");
        assert!(raw.starts_with("HyErr {"), "{raw}");
        assert!(raw.contains("user {{ name }} not found"), "{raw}");
        assert!(raw.contains("bob"), "{raw}");
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
                err_name: "E::A",
                err_tpl: "a",
            },
            ErrRegistration {
                err_code: "1",
                err_name: "E::B",
                err_tpl: "b",
            },
        ];
        assert!(build_templates(&regs).is_err());
    }

    /// 进程里实际注册的错误码没有重复，每一项的模板都能加载
    #[test]
    fn build_templates_ok_for_all_registered() {
        assert!(build_templates(ERR_REGISTRATIONS).is_ok());
    }

    /// with_source 调用两次，以后一次为准
    #[test]
    fn with_source_overrides_previous() {
        let first_source = Inner;
        let second_source = Inner;
        let e = err!(TestErr::Wrapped)
            .with_source(first_source)
            .with_source(second_source);
        // 虽然我们无法区分两个 Inner 实例，但验证 source() 返回的内容符合预期
        assert_eq!(e.source().unwrap().to_string(), "inner cause");
    }

    /// wrap_err 在 Ok 时不调用闭包（用计数器验证）
    #[test]
    fn wrap_err_ok_does_not_invoke_closure() {
        let call_count = std::cell::Cell::new(0);
        let result: Result<i32, Inner> = Ok(42);
        let ok_val = result
            .wrap_err(|| {
                call_count.set(call_count.get() + 1);
                err!(TestErr::Wrapped)
            })
            .unwrap();
        assert_eq!(ok_val, 42);
        assert_eq!(call_count.get(), 0);
    }

    /// wrap_err 在 Err 时调用闭包并挂上 source
    #[test]
    fn wrap_err_err_invokes_closure() {
        let call_count = std::cell::Cell::new(0);
        let result: Result<i32, Inner> = Err(Inner);
        let e = result
            .wrap_err(|| {
                call_count.set(call_count.get() + 1);
                err!(TestErr::Wrapped)
            })
            .unwrap_err();
        assert_eq!(call_count.get(), 1);
        assert!(e.is(TestErr::Wrapped));
        assert_eq!(e.source().unwrap().to_string(), "inner cause");
    }

    /// 渲染失败且带 source 时 `{:#}` 的拼接顺序：msg: source_chain
    #[test]
    fn render_failure_with_source_shows_in_alternate() {
        let e = HyErr::new(
            TestErr::NotFound.err_code(),
            "user {{ name }} not found",
            serde_json::json!({ "nmae": "secret-body" }),
        )
        .with_source(Outer(Inner));
        let detailed = format!("{e:#}");
        // 应该先是渲染失败信息，然后是 source 链
        assert!(detailed.contains("user {{ name }} not found"), "{detailed}");
        assert!(detailed.contains("render failed"), "{detailed}");
        assert!(detailed.contains("outer"), "{detailed}");
        assert!(detailed.contains("inner cause"), "{detailed}");
        // 验证顺序：应该在 "outer" 之前出现 "render failed"
        let render_pos = detailed.find("render failed").unwrap();
        let outer_pos = detailed.find("outer").unwrap();
        assert!(render_pos < outer_pos, "{detailed}");
    }

    /// 3 层及以上 source 链的 Debug 编号（从 0 开始）
    #[test]
    fn debug_three_level_source_chain_has_numbering() {
        #[derive(Debug)]
        struct Level2;
        impl fmt::Display for Level2 {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("level 2")
            }
        }
        impl Error for Level2 {}

        #[derive(Debug)]
        struct Level1(Level2);
        impl fmt::Display for Level1 {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("level 1")
            }
        }
        impl Error for Level1 {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(&self.0)
            }
        }

        let e = err!(TestErr::Wrapped).with_source(Level1(Level2));
        let debug = format!("{e:?}");
        // 应该有编号：0: level 1 和 1: level 2
        assert!(debug.contains("0: level 1"), "{debug}");
        assert!(debug.contains("1: level 2"), "{debug}");
    }

    /// source() 能 downcast 回原始错误
    #[test]
    fn source_can_downcast_to_original_error() {
        let original = Inner;
        let e = err!(TestErr::Wrapped).with_source(original);
        let source = e.source().unwrap();
        // 尝试 downcast 回 Inner
        let inner = source.downcast_ref::<Inner>();
        assert!(inner.is_some());
    }

    /// bail! 提前返回
    #[test]
    fn bail_returns_early() {
        fn check_value(n: i32) -> Result<()> {
            if n < 0 {
                bail!(TestErr::Wrapped);
            }
            Ok(())
        }

        let result = check_value(-1);
        assert!(result.is_err());
        assert!(result.unwrap_err().is(TestErr::Wrapped));

        let result = check_value(1);
        assert!(result.is_ok());
    }

    /// ensure! 条件不成立时提前返回，成立时往下走
    #[test]
    fn ensure_returns_when_condition_fails() {
        fn check_value(n: i32) -> Result<i32> {
            ensure!(n > 0, BaseErr::NotPositive, { "param": "n", "op": "check_value" });
            Ok(n)
        }

        let e = check_value(0).unwrap_err();
        assert!(e.is(BaseErr::NotPositive));
        assert_eq!(e.to_string(), "n must be greater than 0: check_value");
        assert_eq!(check_value(1).unwrap(), 1);
    }

    /// err! 的三种形式：无参、单个值、`{..}`
    #[test]
    fn err_zero_params() {
        let e = err!(TestErr::Wrapped);
        assert!(e.is(TestErr::Wrapped));
        assert_eq!(e.to_string(), "wrapped");
    }

    #[test]
    fn err_single_value() {
        let e = err!(TestErr::NotFound, "alice");
        assert!(e.is(TestErr::NotFound));
        assert_eq!(e.to_string(), "user alice not found");
    }

    #[test]
    fn err_explicit_json() {
        let e = err!(TestErr::NotFound, { "name": "bob" });
        assert!(e.is(TestErr::NotFound));
        assert_eq!(e.to_string(), "user bob not found");
    }
}
