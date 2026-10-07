//! `AxumComponent`：声明的资源、经 Registry 启动后能接请求、缺 router 时启动失败、
//! router 函数取 `before_activate` 放进去的 state（及取不到时启动失败），
//! 以及绕开 Registry 手动驱动的完整生命周期（绑端口、真实请求、取消后优雅结束）
//!
//! 运行：`cargo test -p hygiea-http --features axum --test component`

#![cfg(feature = "axum")]

use std::error::Error as _;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use hygiea_core::HyErr;
use hygiea_core::app::{
    AppErr, CancellationToken, Component, DeferredComponent, Name, ReadyResources, Registry,
    ResourceSink, Resources,
};
use hygiea_http::{AxumComponent, AxumConfig, arity0_state, arity1_state};

/// 先绑一个空闲端口拿到地址，再释放，交给 `AxumComponent` 自己 bind
async fn free_addr() -> std::net::SocketAddr {
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定探测端口");
    probe.local_addr().expect("读取探测端口地址")
}

/// 监听 `addr`，带一个 `GET /ping` 返回 "pong" 的 router
fn ping_config(addr: std::net::SocketAddr) -> AxumConfig {
    AxumConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        router: Some(Box::new(|_| Ok(Router::new().route("/ping", get(|| async { "pong" }))))),
        ..Default::default()
    }
}

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

/// HTTP 服务不往 Resources 里放东西，也不依赖别的资源
#[test]
fn declares_no_resources() {
    let component = AxumComponent::build(Name::from(""), AxumConfig::default());
    assert!(component.provides().is_empty());
    assert!(component.depends_on().is_empty());
}

/// 经 Registry 启动：`on_ready` 时已经在接请求
#[tokio::test]
async fn serves_request_after_registry_startup() {
    let addr = free_addr().await;
    let registry = Registry::new().add::<AxumComponent>(ping_config(addr));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = registry
            .on_ready(move |_| async move {
                let _ = tx.send(());
                Ok(())
            })
            .run()
            .await;
    });
    rx.await.unwrap();

    let resp = reqwest::get(format!("http://{addr}/ping")).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "pong");
    task.abort();
}

/// 没配 router：`prepare` 报错，Registry 启动失败
#[tokio::test]
async fn missing_router_fails_startup() {
    let addr = free_addr().await;
    let config = AxumConfig {
        router: None,
        ..ping_config(addr)
    };
    let (result, _guard) = Registry::new().add::<AxumComponent>(config).run().await;
    // Registry 把组件的错误包成 ComponentStartFailed，组件自己报的 ConfigMissing 挂在 source 上
    let err = result.unwrap_err();
    assert!(err.is(AppErr::ComponentStartFailed), "{err:#}");
    let source = err
        .source()
        .and_then(|e| e.downcast_ref::<HyErr>())
        .unwrap();
    assert!(source.is(AppErr::ConfigMissing), "{err:#}");
}

/// router 函数在 activate 时调用：能取到 `before_activate` 放进去的 state，交给 `with_state`，
/// handler 用 `arity0_state` / `arity1_state` 拿到它
#[tokio::test]
async fn router_fn_reads_state_put_in_before_activate() {
    #[derive(Clone)]
    struct Greeting(String);

    async fn hello(state: Greeting) -> Result<String, HyErr> {
        Ok(state.0)
    }

    async fn echo(state: Greeting, name: String) -> Result<String, HyErr> {
        Ok(format!("{}, {name}", state.0))
    }

    let addr = free_addr().await;
    let config = AxumConfig {
        router: Some(Box::new(|resources| {
            let state = resources.require::<Greeting>()?;
            Ok(Router::new()
                .route("/hello", get(arity0_state(hello)))
                .route("/echo", post(arity1_state(echo)))
                .with_state(state))
        })),
        ..ping_config(addr)
    };
    let registry = Registry::new()
        .add::<AxumComponent>(config)
        .before_activate(|resources| async move {
            resources.insert(Greeting("hi".to_string()));
            Ok(())
        });
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = registry
            .on_ready(move |_| async move {
                let _ = tx.send(());
                Ok(())
            })
            .run()
            .await;
    });
    rx.await.unwrap();

    let hello: serde_json::Value = reqwest::get(format!("http://{addr}/hello"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hello["data"], "hi");
    let echo: serde_json::Value = reqwest::Client::new()
        .post(format!("http://{addr}/echo"))
        .json(&"Bob")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(echo["data"], "hi, Bob");
    task.abort();
}

/// router 函数返回 Err（比如要的 state 没放进去）：activate 失败，Registry 启动失败
#[tokio::test]
async fn router_fn_error_fails_startup() {
    #[derive(Clone)]
    struct Missing;

    let addr = free_addr().await;
    let config = AxumConfig {
        router: Some(Box::new(|resources| {
            let _ = resources.require::<Missing>()?;
            Ok(Router::new())
        })),
        ..ping_config(addr)
    };
    let (result, _guard) = Registry::new().add::<AxumComponent>(config).run().await;
    let err = result.unwrap_err();
    assert!(err.is(AppErr::ComponentActivateFailed), "{err:#}");
}

/// 完整生命周期：绕开 Registry 手动调 `build` / `prepare` / `activate`，自己持有
/// `CancellationToken` 取消，确认后台任务能优雅结束。
///
/// `Resources::new` 对外不可见，外部 crate 拿到一份可用 `Resources` 的唯一途径是 Registry 递给
/// 回调的那份，所以先用空 Registry 把它捞出来
#[tokio::test]
async fn serves_real_request_then_shuts_down_on_cancel() {
    let resources = with_resources(Registry::new(), |res| res).await;
    let addr = free_addr().await;

    let mut component = AxumComponent::build(Name::from(""), ping_config(addr));
    let shutdown = CancellationToken::new();
    component
        .prepare(&ResourceSink::new(&resources))
        .await
        .expect("prepare 应该成功");
    let handle = component
        .activate(ReadyResources::new(&resources), shutdown.clone())
        .await
        .expect("activate 应该成功")
        .expect("axum 组件总是有后台任务");

    let resp = reqwest::get(format!("http://{addr}/ping"))
        .await
        .expect("请求应该成功");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "pong");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("join handle 应该在取消后很快结束")
        .expect("后台任务不应该 panic");
}
