use linkme::distributed_slice;
use minijinja::{Environment, UndefinedBehavior};
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::OnceLock;

static ERROR_TEMPLATES: OnceLock<Environment<'static>> = OnceLock::new();

pub struct ErrRegistration {
    pub err_code: &'static str,
    pub err_tpl: &'static str,
}

/// 成功响应的 code。不是错误，所以不走 `hy_err`；错误码不能占用它，[`init`] 启动时校验
pub const SUCCESS_CODE: &str = "00000000";

#[distributed_slice]
pub static ERR_REGISTRATIONS: [ErrRegistration] = [..];

pub fn init() {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);

    let mut seen_err_codes = HashSet::new();
    for reg in ERR_REGISTRATIONS {
        if reg.err_code == SUCCESS_CODE {
            panic!("err_code {SUCCESS_CODE} is reserved for success");
        }
        if !seen_err_codes.insert(reg.err_code) {
            panic!("Duplicate err_code detected: {}", reg.err_code);
        }
        env.add_template(reg.err_code, reg.err_tpl)
            .unwrap_or_else(|e| panic!("template registration failed: {}", e));
    }

    ERROR_TEMPLATES
        .set(env)
        .unwrap_or_else(|_| panic!("error templates already initialized"));
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
#[derive(Debug)]
pub struct HyErr {
    pub err_code: &'static str,
    pub err_tpl: &'static str,
    pub err_args: serde_json::Value,
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
        let output = ERROR_TEMPLATES
            .get()
            .expect("error templates are not initialized")
            .get_template(self.err_code)
            .and_then(|template| template.render(&self.err_args))
            .unwrap_or_else(|e| {
                format!(
                    "[render failed. err_code: {}, err_tpl: {}, err_args: {}, cause: {}]",
                    self.err_code, self.err_tpl, self.err_args, e
                )
            });
        f.write_str(&output)?;
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
/// 值可以是任何 `Serialize` 类型；key 对不对、值是什么，交给 minijinja 渲染时判断。
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
}
