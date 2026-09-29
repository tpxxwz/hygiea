//! 测试里的日志。
//!
//! [`init_once`] 装的是进程级的全局 subscriber，适合手动跑的联网测试（`--ignored --nocapture`），
//! 用来看请求和响应的日志。自动跑的测试不要调它：日志直接写 stdout，不会被 `cargo test` 捕获；
//! 同一个进程里只有第一次生效，哪个测试先跑也不固定。

use std::sync::OnceLock;

use hygiea::log::{LogGuard, init_default};

/// 装一次 hygiea 的默认日志，一个进程里多次调用只有第一次生效。装失败（比如别处已经装了
/// subscriber）时忽略，测试照常跑，只是看不到日志
pub fn init_once() {
    static LOG_GUARD: OnceLock<Option<LogGuard>> = OnceLock::new();
    LOG_GUARD.get_or_init(|| init_default().ok());
}
