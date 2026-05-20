// 让宏生成的代码能找到 ::hygiea:: 路径
extern crate self as hygiea;

// ========== Feature: format ==========
#[cfg(feature = "format")]
pub use util::format::{tpl_cached, tpl_once, tpl_pos};

// ========== error ==========
mod error;

#[doc(hidden)]
pub use error::__private;
pub use error::{
    BaseFmtErr, BaseRawErr, ERR_REGISTRATIONS, ErrRegistration, ErrRegistrationKind, FmtErr,
    RawErr, SuccessRawErr, fmt_err, raw_err,
};

#[ctor::ctor(unsafe)]
fn init_hygiea_error_env() {
    error::init();
}

// ========== Feature: app ==========
#[cfg(feature = "app")]
pub mod app;

#[cfg(feature = "app")]
pub mod log;

#[cfg(feature = "app")]
pub use app::{Component, LaunchError, Registry, RegistryConfig, Resources};

#[cfg(feature = "app")]
pub use log::{ConsoleLayer, FileLayer, TracingConfig};

// ========== date ==========
pub mod date;
pub use date::{DateFormat, DateTimeUtcExt, Formatter, OffsetDateTime, OffsetDateTimeExt, now_utc, set_now_utc};

// ========== sim ==========
#[cfg(feature = "sim")]
pub mod sim;
#[cfg(feature = "sim")]
pub use sim::{SimClock, TokenBucket};

// ========== ext / util ==========
pub mod ext;
pub mod util;
pub use util::env;
pub use util::{BuiltinKey, EnvKey, env_get, env_get_opt, env_get_or, env_get_or_else};

#[cfg(feature = "distributed-lock")]
pub use ext::{DistributedKey, DistributedLock};
