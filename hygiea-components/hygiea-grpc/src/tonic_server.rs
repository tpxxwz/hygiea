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
pub type TonicServeFn = Box<dyn FnOnce(&ReadyResources) -> Result<TonicServe> + Send + 'static>;

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
}

impl Default for TonicConfig {
    fn default() -> Self {
        Self {
            addr: "0.0.0.0:50051".to_string(),
            shutdown_timeout_secs: None,
        }
    }
}

/// [`TonicComponent`] 的配置：配置文件里的部分（[`TonicConfig`]）加上代码里给的 [`TonicServeFn`]
pub struct TonicComponentConfig {
    pub config: TonicConfig,
    pub serve_fn: TonicServeFn,
}

// ---- component --------------------------------------------------------------

/// gRPC 服务组件，Deferred：`prepare` 校验配置、绑定端口，`activate` 才开始接请求
pub struct TonicComponent {
    config: TonicConfig,
    /// `activate` 里调用一次后取走
    serve_fn: Option<TonicServeFn>,
    /// `prepare` 绑好的端口，`activate` 里交给后台任务
    listener: Option<tokio::net::TcpListener>,
}

#[component]
impl DeferredComponent for TonicComponent {
    type Config = TonicComponentConfig;

    fn build(_name: Name, config: Self::Config) -> Self {
        Self {
            config: config.config,
            serve_fn: Some(config.serve_fn),
            listener: None,
        }
    }

    fn shutdown_timeout(&self) -> Option<std::time::Duration> {
        self.config
            .shutdown_timeout_secs
            .map(std::time::Duration::from_secs)
    }

    async fn prepare(&mut self, _sink: &ResourceSink) -> Result<()> {
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
        self.listener = Some(listener);
        Ok(())
    }

    async fn activate(
        &mut self,
        resources: ReadyResources,
        shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>> {
        let (Some(listener), Some(serve_fn)) = (self.listener.take(), self.serve_fn.take()) else {
            return Err(err!(
                AppErr::ComponentError,
                "TonicComponent activated before prepare"
            ));
        };
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

    /// 配置模板 `config/tonic.toml` 就是代码里的默认值：按配置结构体解析（deny_unknown_fields，写错、多写的项解析失败）。
    /// TonicConfig 里有建 router 的函数，不实现 Debug，逐项比较
    #[test]
    fn template_is_code_defaults() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Template {
            grpc: TonicConfig,
        }
        let template: Template = toml::from_str(include_str!("../config/tonic.toml")).unwrap();
        let default = TonicConfig::default();
        assert_eq!(template.grpc.addr, default.addr);
        assert_eq!(
            template.grpc.shutdown_timeout_secs,
            default.shutdown_timeout_secs
        );
    }

    /// 不往 Resources 里放东西，也不依赖别的资源
    #[test]
    fn provides_and_depends_on_nothing() {
        let config = TonicComponentConfig {
            config: TonicConfig::default(),
            serve_fn: noop_serve_fn(),
        };
        let component = TonicComponent::build(hygiea_core::app::Name::from(""), config);
        assert!(component.provides().is_empty());
        assert!(component.depends_on().is_empty());
    }

    fn noop_serve_fn() -> TonicServeFn {
        Box::new(|_resources: &ReadyResources| {
            Ok(Box::new(
                |_incoming: tokio_stream::wrappers::TcpListenerStream,
                 _shutdown: CancellationToken| {
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
            TonicComponentConfig {
                config: TonicConfig {
                    shutdown_timeout_secs: Some(7),
                    ..Default::default()
                },
                serve_fn: noop_serve_fn(),
            },
        );
        assert_eq!(component.shutdown_timeout(), Some(Duration::from_secs(7)));
    }

    #[test]
    fn shutdown_timeout_none_when_not_set() {
        let component = TonicComponent::build(
            Name::from(""),
            TonicComponentConfig {
                config: TonicConfig::default(),
                serve_fn: noop_serve_fn(),
            },
        );
        assert_eq!(component.shutdown_timeout(), None);
    }

    #[tokio::test]
    async fn invalid_addr_reports_invalid_config() {
        let resources = real_resources().await;
        let mut component = TonicComponent::build(
            Name::from(""),
            TonicComponentConfig {
                config: TonicConfig {
                    addr: "not-an-address".to_string(),
                    ..Default::default()
                },
                serve_fn: noop_serve_fn(),
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
            TonicComponentConfig {
                config: TonicConfig {
                    addr: addr.to_string(),
                    ..Default::default()
                },
                serve_fn: noop_serve_fn(),
            },
        );
        let err = component
            .prepare(&ResourceSink::new(&resources))
            .await
            .expect_err("端口被占用应该启动失败");
        assert!(err.is(AppErr::BindFailed));
    }
}
