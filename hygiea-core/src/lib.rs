// 宏生成的代码按调用方依赖的名字引用本 crate（见 hygiea-macros 的 krate.rs），
// 在 hygiea-core 自己内部就是 ::hygiea_core
extern crate self as hygiea_core;

// ========== 常开 ==========
pub mod datetime;
pub mod env;
mod error;
// hygiea 自己的组件用（db 建表时的锁等），业务用不到；facade 的 glob 会带出去，文档里藏起来
#[doc(hidden)]
pub mod builtin;
pub mod string;

// 错误体系的核心类型放在根上：宏展开后按 ::hygiea::HyErr 等路径引用
#[doc(hidden)]
pub use error::__private;
// err! / bail! 由 #[macro_export] 导出在 crate 根
pub use error::{BaseErr, ErrKind, HyErr, Result, ResultExt, SUCCESS_CODE, hy_err};

#[ctor::ctor(unsafe)]
fn init_hygiea() {
    error::init();

    #[cfg(feature = "datetime-iana")]
    datetime::local::init();

    #[cfg(feature = "lock")]
    lock::init();
}

// ========== 按 feature ==========
#[cfg(feature = "redact")]
pub mod redact;

#[cfg(feature = "log")]
pub mod log;

#[cfg(feature = "app")]
pub mod app;

#[cfg(feature = "lock")]
pub mod lock;

#[cfg(feature = "ws-client")]
pub mod net;
