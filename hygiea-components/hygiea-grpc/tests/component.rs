//! `TonicComponent`：按配置构造、经 Registry 两阶段启动，以及取消后后台任务能 join。
//! 不发真实 gRPC 请求，`serve_fn` 用自定义的闭包代替 tonic Router。
//!
//! 运行：`cargo test -p hygiea-grpc --features tonic --test component`

#![cfg(feature = "tonic")]

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use hygiea_core::app::{
    CancellationToken, Component, DeferredComponent, Name, ReadyResources, Registry, ResourceSink,
    Resources,
};
use hygiea_grpc::{TonicComponent, TonicConfig, TonicServeFn};

/// 启动 registry，把 `Resources` 交给 `check`，拿回它的结果。
/// 启动成功后 `run` 会一直等退出信号，所以放到后台任务里，拿到结果就 abort
async fn with_resources<T: Send + 'static>(
    registry: Registry,
    check: impl FnOnce(Resources) -> T + Send + 'static,
) -> T {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = registry
            .on_ready(move |resources| async move {
                let _ = tx.send(check(resources));
                Ok(())
            })
            .run()
            .await;
    });
    let result = rx.await.unwrap();
    task.abort();
    result
}

/// 被调用时先发个信号，再等取消信号退出
fn signal_then_wait_serve_fn(started: tokio::sync::oneshot::Sender<()>) -> TonicServeFn {
    Box::new(
        |_incoming: tokio_stream::wrappers::TcpListenerStream, shutdown: CancellationToken| {
            Box::pin(async move {
                let _ = started.send(());
                shutdown.cancelled().await;
            }) as Pin<Box<dyn Future<Output = ()> + Send + 'static>>
        },
    )
}

fn local_config(serve_fn: TonicServeFn) -> TonicConfig {
    TonicConfig {
        addr: "127.0.0.1:0".to_string(),
        serve_fn: Some(serve_fn),
        ..Default::default()
    }
}

/// 不往 Resources 里放东西，也不依赖别的资源
#[test]
fn provides_and_depends_on_nothing() {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let component =
        TonicComponent::build(Name::from(""), local_config(signal_then_wait_serve_fn(tx)));
    assert!(component.provides().is_empty());
    assert!(component.depends_on().is_empty());
}

/// 经 Registry 启动：两阶段都成功，`serve_fn` 被调用
#[tokio::test]
async fn registry_starts_and_calls_serve_fn() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let registry =
        Registry::new().add::<TonicComponent>(local_config(signal_then_wait_serve_fn(tx)));
    with_resources(registry, |_| ()).await;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("serve_fn 应该很快被调用")
        .expect("serve_fn 应该发出启动信号");
}

/// 取消 → join：绕开 Registry 手动驱动 `prepare` / `activate`，自己持有 `CancellationToken`。
/// `Resources::new` 对外不可见，用一个空 Registry 启动拿一份可用的 `Resources`
#[tokio::test]
async fn cancel_lets_background_task_join() {
    let resources = with_resources(Registry::new(), |res| res).await;
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let mut component =
        TonicComponent::build(Name::from(""), local_config(signal_then_wait_serve_fn(tx)));

    let shutdown = CancellationToken::new();
    component
        .prepare(&ResourceSink::new(&resources))
        .await
        .expect("prepare 应该成功");
    let handle = component
        .activate(ReadyResources::new(&resources), shutdown.clone())
        .await
        .expect("activate 应该成功")
        .expect("tonic 组件总是有后台任务");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("join handle 应该在取消后很快结束")
        .expect("后台任务不应该 panic");
}
