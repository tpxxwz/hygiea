use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;

use hygiea_core::app::{Component, Resources, async_trait};

// ---- types ------------------------------------------------------------------

pub type TonicServeFn = Box<
    dyn FnOnce(
            tokio_stream::wrappers::TcpListenerStream,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>
        + Send
        + 'static,
>;

// ---- helpers ----------------------------------------------------------------

/// 将一个 tonic Router 表达式包装成 TonicServeFn。
///
/// 用法：`tonic_serve_fn!(create_router())`
#[macro_export]
macro_rules! tonic_serve_fn {
    ($router:expr) => {
        Box::new(|incoming: tokio_stream::wrappers::TcpListenerStream| {
            Box::pin(async move {
                $router
                    .serve_with_incoming(incoming)
                    .await
                    .unwrap_or_else(|e| panic!("gRPC server error: {e}"));
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>
        }) as $crate::TonicServeFn
    };
}

// ---- config -----------------------------------------------------------------

#[derive(Deserialize)]
#[serde(default)]
pub struct TonicConfig {
    pub addr: String,
    #[serde(skip)]
    pub serve_fn: Option<TonicServeFn>,
}

impl Default for TonicConfig {
    fn default() -> Self {
        Self {
            addr: "0.0.0.0:50051".to_string(),
            serve_fn: None,
        }
    }
}

// ---- component --------------------------------------------------------------

pub struct TonicComponent {
    config: TonicConfig,
}

#[async_trait]
impl Component for TonicComponent {
    type Config = TonicConfig;

    fn build(_name: &'static str, config: Self::Config) -> Self {
        Self { config }
    }

    async fn startup(
        &mut self,
        _resources: &Resources,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, anyhow::Error> {
        let serve_fn = self
            .config
            .serve_fn
            .take()
            .ok_or_else(|| anyhow::anyhow!("TonicConfig.serve_fn is not set"))?;

        let addr = self
            .config
            .addr
            .parse::<std::net::SocketAddr>()
            .map_err(|e| anyhow::anyhow!("Invalid gRPC address '{}': {}", self.config.addr, e))?;

        let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
            anyhow::anyhow!("Failed to bind gRPC server to {}: {}", self.config.addr, e)
        })?;

        tracing::info!("gRPC server listener bound to {}", self.config.addr);

        let addr_str = self.config.addr.clone();
        let handle = tokio::spawn(async move {
            tracing::info!("gRPC server serving on {}", addr_str);

            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

            tokio::select! {
                _ = serve_fn(incoming) => {}
                _ = shutdown_rx.recv() => {
                    tracing::info!("gRPC server graceful shutdown initiated");
                }
            }

            tracing::info!("gRPC server stopped");
        });

        Ok(Some(handle))
    }
}
