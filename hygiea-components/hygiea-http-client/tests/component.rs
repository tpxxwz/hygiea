//! `ReqwestComponent` 经 Registry 启动：按组件名提供 `Client`，配置在启动时生效
//!
//! 运行：`cargo test -p hygiea-http-client --features reqwest --test component`

use std::error::Error as _;

use hygiea_core::HyErr;
use hygiea_core::app::{BaseAppErr, Component, Name, Registry, ResourceId, Resources};
use hygiea_http_client::reqwest_client::{
    Client, Json, Method, RequestConfig, ReqwestComponent, ReqwestConfig,
};
use test_support::http_server::{echo, serve};

/// 不走系统代理的配置：开发机上配了系统代理时，请求本地服务也可能被代理截走
fn local_config() -> ReqwestConfig {
    ReqwestConfig {
        no_proxy: true,
        ..ReqwestConfig::default()
    }
}

/// 按配置构造：按组件名声明 Client 资源
#[test]
fn build_declares_named_client() {
    let component = ReqwestComponent::build(Name::from("moji"), local_config());
    assert_eq!(
        component.provides(),
        vec![ResourceId::named::<Client>("moji")]
    );
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

/// 具名添加：Client 注册在组件名下，不占匿名位置
#[tokio::test]
async fn named_component_provides_named_client() {
    let registry = Registry::new().add_named::<ReqwestComponent>("moji", local_config());
    let (named, anonymous) = with_resources(registry, |res| {
        (
            res.get_named::<Client>("moji").is_some(),
            res.get::<Client>().is_some(),
        )
    })
    .await;
    assert!(named);
    assert!(!anonymous);
}

/// 匿名添加：`resources.get::<Client>()` 就能取到
#[tokio::test]
async fn anonymous_component_provides_client() {
    let registry = Registry::new().add::<ReqwestComponent>(local_config());
    assert!(with_resources(registry, |res| res.get::<Client>().is_some()).await);
}

/// 同类型多个实例各用各的配置
#[tokio::test]
async fn each_named_client_uses_its_own_config() {
    let base = serve(echo).await;
    let config = |ua: &str| ReqwestConfig {
        user_agent: Some(ua.into()),
        ..local_config()
    };
    let registry = Registry::new()
        .add_named::<ReqwestComponent>("a", config("ua-a"))
        .add_named::<ReqwestComponent>("b", config("ua-b"));
    let (a, b) = with_resources(registry, |res| {
        (res.get_named::<Client>("a"), res.get_named::<Client>("b"))
    })
    .await;
    for (client, ua) in [(a.unwrap(), "ua-a"), (b.unwrap(), "ua-b")] {
        let seen = RequestConfig::plain(Method::GET, format!("{base}/echo"))
            .send::<Json<serde_json::Value>>(&client)
            .await
            .unwrap()
            .body;
        assert_eq!(seen["headers"]["user-agent"], ua);
    }
}

/// 配置建不出 Client 时启动失败：Registry 报 ComponentStartFailed，
/// source 是组件报的 BaseAppErr::InvalidConfig；业务回调不会执行
#[tokio::test]
async fn invalid_config_fails_startup() {
    let config = ReqwestConfig {
        user_agent: Some("bad\nagent".into()),
        ..local_config()
    };
    let (result, _guard) = Registry::new()
        .add::<ReqwestComponent>(config)
        .on_ready(|_| async { panic!("init callback must not run") as Result<(), HyErr> })
        .run()
        .await;
    let err = result.unwrap_err();
    assert!(err.is(BaseAppErr::ComponentStartFailed), "{err:#}");
    let source = err
        .source()
        .and_then(|e| e.downcast_ref::<HyErr>())
        .unwrap();
    assert!(source.is(BaseAppErr::InvalidConfig), "{err:#}");
}
