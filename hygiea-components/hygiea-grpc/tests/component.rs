//! `TonicComponent`：按配置构造、经 Registry 两阶段启动、`tonic_serve_fn!` 取资源（及取不到时启动失败），
//! 以及取消后后台任务能 join。不发真实 gRPC 请求，用自定义的闭包、`FakeRouter` 代替 tonic Router。
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
use hygiea_core::HyErr;
use hygiea_core::app::AppErr;
use hygiea_grpc::{TonicComponent, TonicConfig, TonicServe, TonicServeFn, tonic_serve_fn};

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
    Box::new(|_resources: &ReadyResources| {
        Ok(Box::new(
            |_incoming: tokio_stream::wrappers::TcpListenerStream, shutdown: CancellationToken| {
                Box::pin(async move {
                    let _ = started.send(());
                    shutdown.cancelled().await;
                }) as Pin<Box<dyn Future<Output = ()> + Send + 'static>>
            },
        ) as TonicServe)
    })
}

/// 代替 tonic Router：只有 `tonic_serve_fn!` 展开后调用的 `serve_with_incoming_shutdown`。
/// 被调用时把建它时拿到的值发出去，再等退出信号
struct FakeRouter {
    value: String,
    started: tokio::sync::oneshot::Sender<String>,
}

impl FakeRouter {
    async fn serve_with_incoming_shutdown<F: Future<Output = ()>>(
        self,
        _incoming: tokio_stream::wrappers::TcpListenerStream,
        shutdown: F,
    ) -> Result<(), std::io::Error> {
        let _ = self.started.send(self.value);
        shutdown.await;
        Ok(())
    }
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

/// `tonic_serve_fn!(|resources| ..)`：activate 时能取到 `before_activate` 放进去的资源
#[tokio::test]
async fn serve_fn_macro_reads_resources() {
    #[derive(Clone)]
    struct Greeting(String);

    let (tx, rx) = tokio::sync::oneshot::channel();
    let serve_fn = tonic_serve_fn!(|resources| FakeRouter {
        value: resources.require::<Greeting>()?.0,
        started: tx,
    });
    let registry = Registry::new()
        .add::<TonicComponent>(local_config(serve_fn))
        .before_activate(|resources| async move {
            resources.insert(Greeting("hi".to_string()));
            Ok(())
        });
    with_resources(registry, |_| ()).await;
    let value = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("服务应该很快被调用")
        .expect("服务应该发出取到的值");
    assert_eq!(value, "hi");
}

/// `tonic_serve_fn!(expr)`：不需要资源的写法照旧能用
#[tokio::test]
async fn serve_fn_macro_without_resources() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let router = FakeRouter {
        value: "plain".to_string(),
        started: tx,
    };
    let registry = Registry::new().add::<TonicComponent>(local_config(tonic_serve_fn!(router)));
    with_resources(registry, |_| ()).await;
    let value = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("服务应该很快被调用")
        .expect("服务应该发出值");
    assert_eq!(value, "plain");
}

/// serve_fn 返回 Err（比如要的资源没放进去）：activate 失败，Registry 启动失败
#[tokio::test]
async fn serve_fn_error_fails_startup() {
    #[derive(Clone)]
    struct Missing;

    let (tx, _rx) = tokio::sync::oneshot::channel();
    let serve_fn = tonic_serve_fn!(|resources| {
        let _ = resources.require::<Missing>()?;
        FakeRouter {
            value: String::new(),
            started: tx,
        }
    });
    let (result, _guard) = Registry::new()
        .add::<TonicComponent>(local_config(serve_fn))
        .run()
        .await;
    let err: HyErr = result.unwrap_err();
    assert!(err.is(AppErr::ComponentActivateFailed), "{err:#}");
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
