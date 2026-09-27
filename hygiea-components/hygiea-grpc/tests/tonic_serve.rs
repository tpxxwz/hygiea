//! `TonicComponent` 的取消 → join：自定义一个收到取消信号就退出的 `serve_fn`，验证取消之后
//! 后台任务能正常 join。
//!
//! 不建 `Registry` 来跑被测组件本身：`Resources::new` 对外不可见，外部 crate 拿到一份可用
//! `Resources` 的唯一公开途径是 `Registry::run` 递给回调的那份。这里用一个什么都不做的占位组件
//! 把 Registry 拉起来，只是为了从回调里把 `Resources` 捞出来；真正要测的 `TonicComponent` 完全绕开
//! Registry，自己直接调 `Component::build` / `startup`，自己持有 `CancellationToken` 手动取消。

#![cfg(feature = "tonic")]

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use hygiea_core::HyErr;
use hygiea_core::app::{CancellationToken, Component, Name, Registry, Resources, async_trait};
use hygiea_grpc::{TonicComponent, TonicConfig, TonicServeFn};

struct NoopComponent;

#[async_trait]
impl Component for NoopComponent {
    type Config = ();

    fn build(_name: Name, _config: ()) -> Self {
        Self
    }

    async fn startup(
        &mut self,
        _resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, HyErr> {
        Ok(None)
    }
}

/// 拿一份真正可用的 `Resources`，见文件头的说明
async fn real_resources() -> Resources {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(
        Registry::new()
            .add::<NoopComponent>(())
            .run(move |resources| async move {
                let _ = tx.send(resources);
                Ok(())
            }),
    );
    rx.await.expect("registry 应该把 resources 递回来")
}

/// 收到取消信号就退出，不做别的事
fn exit_on_cancel_serve_fn() -> TonicServeFn {
    Box::new(
        |_incoming: tokio_stream::wrappers::TcpListenerStream, shutdown: CancellationToken| {
            Box::pin(async move {
                shutdown.cancelled().await;
            }) as Pin<Box<dyn Future<Output = ()> + Send + 'static>>
        },
    )
}

#[tokio::test]
async fn cancel_lets_background_task_join() {
    let resources = real_resources().await;

    let mut component = TonicComponent::build(
        Name::from(""),
        TonicConfig {
            addr: "127.0.0.1:0".to_string(),
            serve_fn: Some(exit_on_cancel_serve_fn()),
            ..Default::default()
        },
    );

    let shutdown = CancellationToken::new();
    let handle = component
        .startup(&resources, shutdown.clone())
        .await
        .expect("组件应该启动成功")
        .expect("tonic 组件总是有后台任务");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("join handle 应该在取消后很快结束")
        .expect("后台任务不应该 panic");
}
