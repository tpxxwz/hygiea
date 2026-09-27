//! 进程退出信号（Linux / macOS / Windows）

/// 进程的退出信号监听，`run` 一开始装好，整个运行期间用同一个。
///
/// - Linux / macOS：SIGINT（Ctrl+C）、SIGTERM（`docker stop`、k8s 删 Pod 发的都是它）
/// - Windows：Ctrl+C、Ctrl+Break、关闭控制台窗口、系统关机（Windows 没有 SIGTERM）
///
/// 装好之后这些信号不再按系统默认行为直接结束进程，而是交给框架走优雅关闭。
/// 某个信号装不上（极少见）只打 ERROR，不影响其他信号
pub(super) struct ShutdownSignals {
    #[cfg(unix)]
    interrupt: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    terminate: Option<tokio::signal::unix::Signal>,
    #[cfg(windows)]
    ctrl_c: Option<tokio::signal::windows::CtrlC>,
    #[cfg(windows)]
    ctrl_break: Option<tokio::signal::windows::CtrlBreak>,
    #[cfg(windows)]
    ctrl_close: Option<tokio::signal::windows::CtrlClose>,
    #[cfg(windows)]
    ctrl_shutdown: Option<tokio::signal::windows::CtrlShutdown>,
}

impl ShutdownSignals {
    pub(super) fn install() -> Self {
        fn ok_or_log<T>(name: &str, result: std::io::Result<T>) -> Option<T> {
            result
                .inspect_err(|e| tracing::error!("Failed to listen for {name}: {e}"))
                .ok()
        }
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                interrupt: ok_or_log("Ctrl+C (SIGINT)", signal(SignalKind::interrupt())),
                terminate: ok_or_log("SIGTERM", signal(SignalKind::terminate())),
            }
        }
        #[cfg(windows)]
        {
            use tokio::signal::windows;
            Self {
                ctrl_c: ok_or_log("Ctrl+C", windows::ctrl_c()),
                ctrl_break: ok_or_log("Ctrl+Break", windows::ctrl_break()),
                ctrl_close: ok_or_log("console close", windows::ctrl_close()),
                ctrl_shutdown: ok_or_log("system shutdown", windows::ctrl_shutdown()),
            }
        }
    }

    /// 等下一个退出信号，返回它的名字（打日志用）
    pub(super) async fn recv(&mut self) -> &'static str {
        // 没装上的信号永远等不到
        macro_rules! wait {
            ($signal:expr) => {
                async {
                    match $signal.as_mut() {
                        Some(signal) => {
                            signal.recv().await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                }
            };
        }
        #[cfg(unix)]
        {
            tokio::select! {
                () = wait!(self.interrupt) => "Ctrl+C (SIGINT)",
                () = wait!(self.terminate) => "SIGTERM",
            }
        }
        #[cfg(windows)]
        {
            tokio::select! {
                () = wait!(self.ctrl_c) => "Ctrl+C",
                () = wait!(self.ctrl_break) => "Ctrl+Break",
                () = wait!(self.ctrl_close) => "console close",
                () = wait!(self.ctrl_shutdown) => "system shutdown",
            }
        }
    }
}

/// 关闭过程中又收到信号：直接退出进程，退出码 130（惯例：被 SIGINT 结束）。
///
/// k8s 只发一次 SIGTERM，到点直接 SIGKILL，用不到这条；主要给本地开发：关闭卡住时
/// 再按一次 Ctrl+C 就能退出，不用另开终端 kill -9。`process::exit` 不跑析构函数，
/// 文件日志缓冲里最后几行可能写不进去，所以同时打到 stderr
pub(super) fn force_exit(signal: &str) -> ! {
    tracing::error!("Received {signal} again during shutdown, forcing exit");
    eprintln!("ERROR hygiea: received {signal} again during shutdown, forcing exit");
    std::process::exit(130)
}
