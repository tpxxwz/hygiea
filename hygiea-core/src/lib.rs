//! hygiea 的核心实现：应用框架、错误、日志、日期时间、环境变量、字符串、分布式锁的抽象等。使用方经 facade crate `hygiea` 使用，不直接依赖它。

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

// hygiea-db 动态配置的 #[derive(hy_live_config)]：由 hygiea-db 的 live_config 模块导出（hygiea::db::live_config::hy_live_config），
// 过程宏经由 core 导出，这里只转一手；facade 的 glob 会带出去，文档里藏起来
#[cfg(feature = "live-config")]
#[doc(hidden)]
pub mod __live_config {
    pub use hygiea_macros::hy_live_config;
}
