//! 给当前进程发 SIGTERM 触发优雅关闭，验证 `shutdown_delay` 结束之前组件不会被 stop。
//!
//! 只在 unix 上编译：Windows 没有 SIGTERM（见 `signal.rs`）。发信号用 `kill -TERM <pid>`，
//! 避免为了发信号专门加一个依赖。这类"真的等一段时间、真的发系统信号"的测试本质上不稳定
//! （依赖调度器及时把 startup、信号处理跑起来），如果在 CI 上间歇性失败，应该放宽时间窗口
//! 或者干脆跳过，而不是删掉这条覆盖。

#![cfg(all(unix, feature = "app"))]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hygiea_core::Result;
use hygiea_core::app::{
    CancellationToken, ImmediateComponent, Name, Registry, RegistryConfig, Resources, component,
};
use tokio::task::JoinHandle;

/// 只记事件、不做别的的组件：有一个等 shutdown 信号才结束的后台任务
struct Marker(Arc<Mutex<Vec<String>>>);

#[component]
impl ImmediateComponent for Marker {
    type Config = Arc<Mutex<Vec<String>>>;

    fn build(_name: Name, events: Self::Config) -> Self {
        Self(events)
    }

    async fn startup(
        &mut self,
        _state: &Resources,
        shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        self.0.lock().unwrap().push("startup".to_string());
        let events = self.0.clone();
        Ok(Some(tokio::spawn(async move {
            shutdown.cancelled().await;
            events.lock().unwrap().push("task_done".to_string());
        })))
    }

    async fn stop(&mut self) -> Result<()> {
        self.0.lock().unwrap().push("stop".to_string());
        Ok(())
    }
}

#[tokio::test]
async fn shutdown_delay_postpones_component_stop() {
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let registry = Registry::with_config(RegistryConfig {
        shutdown_delay_secs: 1,
        ..RegistryConfig::default()
    })
    .add::<Marker>(events.clone());

    // run 本身要等到收到信号才会往下走，放到后台任务里跑，测试主体负责发信号、在不同时间点检查状态
    let handle = tokio::spawn(registry.run());

    // 给调度器一点时间把 startup 跑完，再发信号
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(events.lock().unwrap().clone(), vec!["startup".to_string()]);

    std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .expect("send SIGTERM to self");

    // 信号发出后、shutdown_delay（1 秒）结束前，组件应该还没被 stop
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        events.lock().unwrap().clone(),
        vec!["startup".to_string()],
        "shutdown_delay 还没结束，组件不应该被 stop"
    );

    // delay 结束之后，run 正常走完关闭流程并返回
    let (result, _guard) = handle.await.expect("registry.run task should not panic");
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        events.lock().unwrap().clone(),
        vec![
            "startup".to_string(),
            "task_done".to_string(),
            "stop".to_string()
        ]
    );
}
