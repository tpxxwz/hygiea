//! 后台任务和优雅关闭：`startup` 返回任务的 `JoinHandle`，关闭时收到 `CancellationToken` 就退出；
//! 不响应的任务等到 `shutdown_timeout` 后被强制结束；`stop` 在任务结束后做收尾。
//!
//! ```bash
//! cargo run -p hygiea-examples --example app_background_task
//! ```
//!
//! 启动后等几秒看 ticker 的日志，再按 Ctrl+C：
//! - `stuck` 不理退出信号，2 秒后被强制结束，打 WARN；
//! - `ticker` 收到信号立即退出，然后调它的 `stop`。
//!
//! 关闭中再按一次 Ctrl+C 直接退出进程。

use std::time::Duration;

use hygiea::HyErr;
use hygiea::app::{CancellationToken, Component, Name, Registry, Resources, async_trait};

// ---- 正常的后台任务：每秒打一行，收到退出信号就结束 ----

pub struct TickerComponent;

#[async_trait]
impl Component for TickerComponent {
    type Config = ();

    fn build(_name: Name, _config: Self::Config) -> Self {
        Self
    }

    async fn startup(
        &mut self,
        _resources: &Resources,
        shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, HyErr> {
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            let mut ticks = 0u32;
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => {
                        tracing::info!("ticker: got shutdown signal after {ticks} ticks");
                        break;
                    }
                    _ = interval.tick() => {
                        ticks += 1;
                        tracing::info!("ticker: tick {ticks}");
                    }
                }
            }
        });
        Ok(Some(handle))
    }

    // 后台任务结束之后调用：关连接、刷缓冲之类的收尾放这里
    async fn stop(&mut self) -> Result<(), HyErr> {
        tracing::info!("ticker: stop() called, cleanup done");
        Ok(())
    }
}

// ---- 不响应退出信号的任务：超时后被强制结束 ----

pub struct StuckComponent;

#[async_trait]
impl Component for StuckComponent {
    type Config = ();

    fn build(_name: Name, _config: Self::Config) -> Self {
        Self
    }

    async fn startup(
        &mut self,
        _resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, HyErr> {
        // 故意不看 shutdown
        let handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
        Ok(Some(handle))
    }

    // 这个组件最多等 2 秒，不写就用 RegistryConfig::shutdown_timeout_secs（默认 15 秒）
    fn shutdown_timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }
}

#[tokio::main]
async fn main() -> Result<(), HyErr> {
    // 没有依赖关系的组件按 add 的顺序启动，关闭时反过来：先关 stuck，再关 ticker
    let (result, _log_guard) = Registry::new()
        .add::<TickerComponent>(())
        .add::<StuckComponent>(())
        .run(|_| async {
            tracing::info!("press Ctrl+C to start graceful shutdown");
            Ok(())
        })
        .await;
    result
}
