//! gRPC 服务组件（tonic），feature `tonic`

use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;

use hygiea_core::app::{
    AppErr, CancellationToken, DeferredComponent, Name, ReadyResources, ResourceSink, component,
};
use hygiea_core::{Result, ResultExt, err};

// ---- types ------------------------------------------------------------------

/// 跑 gRPC 服务：拿到监听好的连接流和退出信号，跑到服务结束。
///
/// 收到退出信号后应该优雅关闭：不再接新请求，等手上的请求处理完再返回。
/// 用 tonic 的 `serve_with_incoming_shutdown(incoming, shutdown.cancelled_owned())` 就是这样，
/// [`tonic_serve_fn!`] 已经这么写好了
pub type TonicServe = Box<
    dyn FnOnce(
            tokio_stream::wrappers::TcpListenerStream,
            CancellationToken,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>
        + Send
        + 'static,
>;

/// 建 gRPC 服务的函数，`activate` 时调用。这时所有组件的资源、`Registry::before_activate` 里放进去的东西都已就绪，
/// 可以从 [`ReadyResources`] 取出连接池、业务 state 建 tonic Router。返回 `Err` 时组件启动失败。
/// 一般用 [`tonic_serve_fn!`] 生成
pub type TonicServeFn =
    Box<dyn FnOnce(&ReadyResources) -> Result<TonicServe> + Send + 'static>;

/// 宏展开后要用，调用方不必自己依赖这些 crate
#[doc(hidden)]
pub mod __private {
    pub use hygiea_core::Result;
    pub use hygiea_core::app::{CancellationToken, ReadyResources};
    pub use tokio_stream::wrappers::TcpListenerStream;
    pub use tracing;
}

// ---- helpers ----------------------------------------------------------------

/// 把建 tonic Router 的表达式包装成 [`TonicServeFn`]，收到退出信号后优雅关闭（处理完手上的请求再退出）。
///
/// - 不需要资源：`tonic_serve_fn!(create_router())`
/// - 要从资源里取东西：`tonic_serve_fn!(|resources| create_router(resources.require::<Arc<AppState>>()?))`，
///   表达式里可以用 `?`（返回 `hygiea::Result`）。参数名自己起，不能写成 `_`
#[macro_export]
macro_rules! tonic_serve_fn {
    (|$resources:ident| $router:expr) => {
        Box::new(
            |$resources: &$crate::__private::ReadyResources|
             -> $crate::__private::Result<$crate::TonicServe> {
                let router = $router;
                Ok(Box::new(
                    move |incoming: $crate::__private::TcpListenerStream,
                          shutdown: $crate::__private::CancellationToken| {
                        Box::pin(async move {
                            if let Err(e) = router
                                .serve_with_incoming_shutdown(incoming, shutdown.cancelled_owned())
                                .await
                            {
                                $crate::__private::tracing::error!("gRPC server error: {e}");
                            }
                        })
                            as std::pin::Pin<
                                Box<dyn std::future::Future<Output = ()> + Send + 'static>,
                            >
                    },
                ) as $crate::TonicServe)
            },
        ) as $crate::TonicServeFn
    };
    ($router:expr) => {
        $crate::tonic_serve_fn!(|_resources| $router)
    };
}

// ---- config -----------------------------------------------------------------

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TonicConfig {
    pub addr: String,
    /// 关闭时最多等多久（秒）让手上的请求处理完，不写用 Registry 的默认值（15 秒）
    pub shutdown_timeout_secs: Option<u64>,
    #[serde(skip)]
    pub serve_fn: Option<TonicServeFn>,
}

impl Default for TonicConfig {
    fn default() -> Self {
        Self {
            addr: "0.0.0.0:50051".to_string(),
            shutdown_timeout_secs: None,
            serve_fn: None,
        }
    }
}

// ---- component --------------------------------------------------------------

/// gRPC 服务组件，Deferred：`prepare` 校验配置、绑定端口，`activate` 才开始接请求
pub struct TonicComponent {
    config: TonicConfig,
    /// `prepare` 绑好的端口和取出的 serve_fn，`activate` 里建服务、交给后台任务
    prepared: Option<(tokio::net::TcpListener, TonicServeFn)>,
}

#[component]
impl DeferredComponent for TonicComponent {
    type Config = TonicConfig;

    fn build(_name: Name, config: Self::Config) -> Self {
        Self {
            config,
            prepared: None,
        }
    }

    fn shutdown_timeout(&self) -> Option<std::time::Duration> {
        self.config
            .shutdown_timeout_secs
            .map(std::time::Duration::from_secs)
    }

    async fn prepare(&mut self, _sink: &ResourceSink) -> Result<()> {
        let serve_fn = self
            .config
            .serve_fn
            .take()
            .ok_or_else(|| err!(AppErr::ConfigMissing, "TonicConfig.serve_fn"))?;

        let addr = self
            .config
            .addr
            .parse::<std::net::SocketAddr>()
            .wrap_err(|| {
                err!(
                    AppErr::InvalidConfig,
                    format!("invalid gRPC address: {}", self.config.addr)
                )
            })?;

        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .wrap_err(|| err!(AppErr::BindFailed, &self.config.addr))?;

        tracing::info!("gRPC server listener bound to {}", self.config.addr);
        self.prepared = Some((listener, serve_fn));
        Ok(())
    }

    async fn activate(
        &mut self,
        resources: ReadyResources,
        shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>> {
        let (listener, serve_fn) = self.prepared.take().ok_or_else(|| {
            err!(
                AppErr::ComponentError,
                "TonicComponent activated before prepare"
            )
        })?;
        let serve = serve_fn(&resources)?;

        let addr_str = self.config.addr.clone();
        let handle = tokio::spawn(async move {
            tracing::info!("gRPC server serving on {}", addr_str);

            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
            // 退出信号交给 serve_fn，由 tonic 自己优雅关闭：不再接新请求，等手上的请求处理完
            serve(incoming, shutdown).await;
            tracing::info!("gRPC server stopped");
        });

        Ok(Some(handle))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hygiea_core::app::{Component, ImmediateComponent, Registry, Resources};

    use super::*;

    fn noop_serve_fn() -> TonicServeFn {
        Box::new(|_resources: &ReadyResources| {
            Ok(Box::new(
                |_incoming: tokio_stream::wrappers::TcpListenerStream, _shutdown: CancellationToken| {
                    Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send + 'static>>
                },
            ) as TonicServe)
        })
    }

    /// `TonicComponent::prepare` 的 `ResourceSink` 要从一份真正可用的 `Resources` 构造，但 `Resources::new` 对外不可见，
    /// 外部（这里是本 crate 的单元测试，和 core 不是同一个 crate）唯一能拿到实例的公开途径是
    /// `Registry::run` 递给回调的那份：用一个什么都不做的占位组件把 Registry 拉起来，从回调里把
    /// `Resources` 捞出来，真正要测的 `TonicComponent` 完全绕开 Registry，自己直接调
    /// `Component::build` / `prepare`
    async fn real_resources() -> Resources {
        struct NoopComponent;

        #[component]
        impl ImmediateComponent for NoopComponent {
            type Config = ();

            fn build(_name: Name, _config: ()) -> Self {
                Self
            }

            async fn startup(
                &mut self,
                _resources: &Resources,
                _shutdown: CancellationToken,
            ) -> Result<Option<tokio::task::JoinHandle<()>>> {
                Ok(None)
            }
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(
            Registry::new()
                .add::<NoopComponent>(())
                .on_ready(move |resources| async move {
                    let _ = tx.send(resources);
                    Ok(())
                })
                .run(),
        );
        rx.await.expect("registry 应该把 resources 递回来")
    }

    #[test]
    fn shutdown_timeout_converts_secs_to_duration() {
        let component = TonicComponent::build(
            Name::from(""),
            TonicConfig {
                shutdown_timeout_secs: Some(7),
                ..Default::default()
            },
        );
        assert_eq!(component.shutdown_timeout(), Some(Duration::from_secs(7)));
    }

    #[test]
    fn shutdown_timeout_none_when_not_set() {
        let component = TonicComponent::build(Name::from(""), TonicConfig::default());
        assert_eq!(component.shutdown_timeout(), None);
    }

    #[tokio::test]
    async fn missing_serve_fn_reports_config_missing() {
        let resources = real_resources().await;
        let mut component = TonicComponent::build(
            Name::from(""),
            TonicConfig {
                addr: "127.0.0.1:0".to_string(),
                serve_fn: None,
                ..Default::default()
            },
        );
        let err = component
            .prepare(&ResourceSink::new(&resources))
            .await
            .expect_err("缺 serve_fn 应该启动失败");
        assert!(err.is(AppErr::ConfigMissing));
    }

    #[tokio::test]
    async fn invalid_addr_reports_invalid_config() {
        let resources = real_resources().await;
        let mut component = TonicComponent::build(
            Name::from(""),
            TonicConfig {
                addr: "not-an-address".to_string(),
                serve_fn: Some(noop_serve_fn()),
                ..Default::default()
            },
        );
        let err = component
            .prepare(&ResourceSink::new(&resources))
            .await
            .expect_err("非法地址应该启动失败");
        assert!(err.is(AppErr::InvalidConfig));
    }

    #[tokio::test]
    async fn port_in_use_reports_bind_failed() {
        let resources = real_resources().await;
        // 先占住一个端口，让 TonicComponent 再 bind 同一个地址时失败
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定占位端口");
        let addr = occupied.local_addr().expect("读取占位端口地址");

        let mut component = TonicComponent::build(
            Name::from(""),
            TonicConfig {
                addr: addr.to_string(),
                serve_fn: Some(noop_serve_fn()),
                ..Default::default()
            },
        );
        let err = component
            .prepare(&ResourceSink::new(&resources))
            .await
            .expect_err("端口被占用应该启动失败");
        assert!(err.is(AppErr::BindFailed));
    }
}
