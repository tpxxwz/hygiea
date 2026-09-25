// 宏生成的代码按调用方依赖的名字引用本 crate（见 hygiea-macros 的 krate.rs），
// 在 hygiea-core 自己内部就是 ::hygiea_core
extern crate self as hygiea_core;

// ========== 常开 ==========
pub mod datetime;
pub mod env;
mod error;
pub mod redact;
pub mod string;
pub mod sync;

// 错误体系的核心类型放在根上：宏展开后按 ::hygiea::HyErr 等路径引用
#[doc(hidden)]
pub use error::__private;
// err! / bail! 由 #[macro_export] 导出在 crate 根
pub use error::{
    BaseErr, ERR_REGISTRATIONS, ErrKind, ErrRegistration, HyErr, ResultExt, SUCCESS_CODE, hy_err,
};

#[ctor::ctor(unsafe)]
fn init_hygiea() {
    error::init();

    #[cfg(feature = "datetime-iana")]
    datetime::iana::init();
}

// ========== 按 feature ==========
#[cfg(feature = "log")]
pub mod log;

#[cfg(feature = "app")]
pub mod app;

#[cfg(any(feature = "http", feature = "ws"))]
pub mod net;
