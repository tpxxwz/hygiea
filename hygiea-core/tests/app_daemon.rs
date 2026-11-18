//! 后台运行和停（feature `app-daemon`）的端到端测试：真的起一个后台应用，再用 `daemon::stop` 停掉它，
//! 确认走的是正常关闭（组件的 `stop` 跑了、pid 文件删了），不是被强制结束。
//!
//! 测试二进制自己当后台应用：父进程用 `spawn_detached` 按同样的命令行再起一个自己，子进程看到环境变量
//! [`CHILD_DIR`] 就不当测试跑，而是起一个 `Registry`。这个文件只能有这一个测试：子进程按父进程的命令行跑，
//! 不带过滤时会把文件里的测试都跑一遍。
//!
//! 运行：`cargo nextest run -p hygiea-core --all-features --test app_daemon`

#![cfg(feature = "app-daemon")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hygiea_core::Result;
use hygiea_core::app::{
    CancellationToken, ImmediateComponent, Name, Registry, RegistryConfig, Resources, component, daemon,
};
use tokio::task::JoinHandle;

/// 子进程用的目录：pid 文件、组件停的时候写的标记文件都在这里
const CHILD_DIR: &str = "HYGIEA_TEST_DAEMON_CHILD_DIR";

/// 停的时候写一个标记文件：文件在就说明 `stop` 跑过，是正常关闭
struct MarkStopComponent {
    marker: PathBuf,
}

#[component]
impl ImmediateComponent for MarkStopComponent {
    type Config = PathBuf;

    fn build(_name: Name, marker: Self::Config) -> Self {
        Self { marker }
    }

    async fn startup(
        &mut self,
        _state: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        Ok(None)
    }

    async fn stop(&mut self) -> Result<()> {
        std::fs::write(&self.marker, "stopped").expect("write stop marker");
        Ok(())
    }
}

#[tokio::test]
async fn stop_shuts_detached_app_down_gracefully() {
    if let Some(dir) = std::env::var_os(CHILD_DIR) {
        run_child_app(PathBuf::from(dir)).await;
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("app.pid");
    let marker = dir.path().join("stopped");
    // 子进程继承环境变量；nextest 一个测试一个进程，这里没有别的线程在读写环境变量
    unsafe { std::env::set_var(CHILD_DIR, dir.path()) };

    let pid = daemon::spawn_detached(&pid_file, Duration::from_millis(500), Duration::from_secs(30)).unwrap();
    assert_eq!(daemon::status(&pid_file), Some(pid));
    assert!(!marker.exists());

    let started = Instant::now();
    assert_eq!(daemon::stop(&pid_file, Duration::from_secs(30)).unwrap(), Some(pid));
    assert!(marker.exists(), "component stop ran: graceful shutdown, not killed");
    assert!(!pid_file.exists(), "pid file removed on exit");
    assert_eq!(daemon::status(&pid_file), None);
    // 正常关闭没有要等的组件，很快就结束
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

/// 子进程：带 pid 文件和 [`MarkStopComponent`] 跑一个应用，直到被 `daemon::stop` 停掉
async fn run_child_app(dir: PathBuf) {
    let config = RegistryConfig {
        pid_file: Some(dir.join("app.pid").display().to_string()),
        ..Default::default()
    };
    let (result, _log_guard) = Registry::with_config(config)
        .add::<MarkStopComponent>(dir.join("stopped"))
        .run()
        .await;
    result.unwrap();
}
