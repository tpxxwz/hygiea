//! `init_default` 能成功初始化（只输出到控制台，级别 info，本地时间）。
//!
//! 单独一个文件、只有一个 `#[test]` 的原因见 `tests/log_file_layer.rs` 开头：日志是进程级全局的，
//! 一种配置一个进程。

#![cfg(feature = "log")]

use hygiea_core::log::init_default;

#[test]
fn init_default_succeeds() {
    let guard = init_default().unwrap();
    // 只是确认装上之后打日志不会 panic
    tracing::info!("hello from init_default");
    drop(guard);
}
