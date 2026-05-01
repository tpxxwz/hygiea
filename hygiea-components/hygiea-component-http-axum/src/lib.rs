use std::time::Duration;

use serde::Deserialize;

use hygiea_core::app::{Component, Resources, async_trait};

// ---- config -----------------------------------------------------------------

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct HttpTimeout {
    pub connect_secs: Option<u64>,
    pub header_read_secs: Option<u64>,
    pub request_read_secs: Option<u64>,
    pub response_write_secs: Option<u64>,
}

impl Default for HttpTimeout {
    fn default() -> Self {
        Self {
            connect_secs: None,
            header_read_secs: None,
            request_read_secs: None,
            response_write_secs: None,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct HttpLimits {
    pub max_body_size: Option<usize>,
    pub max_header_size: Option<usize>,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            max_body_size: None,
            max_header_size: None,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct AxumConfig {
    pub host: String,
    pub port: u16,
    pub base_path: String,
    pub timeout: Option<HttpTimeout>,
    pub limits: Option<HttpLimits>,
    #[serde(skip)]
    pub router: Option<axum::Router>,
}

impl Default for AxumConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            base_path: String::new(),
            timeout: None,
            limits: None,
            router: None,
        }
    }
}

impl AxumConfig {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

// ---- component --------------------------------------------------------------

pub struct AxumComponent {
    config: AxumConfig,
}

#[async_trait]
impl Component for AxumComponent {
    type Config = AxumConfig;

    fn build(_name: &'static str, config: Self::Config) -> Self {
        Self { config }
    }

    async fn startup(
        &mut self,
        _resources: &Resources,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, anyhow::Error> {
        let addr = self.config.addr();
        let router = self
            .config
            .router
            .take()
            .ok_or_else(|| anyhow::anyhow!("AxumConfig.router is not set"))?;
        let config = self.config.clone();

        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind HTTP server to {}: {}", addr, e))?;

        tracing::info!("HTTP server listener bound to {}", addr);

        let handle = tokio::spawn(async move {
            tracing::info!("HTTP server serving on {}", addr);

            let router = apply_layers(router, &config);

            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.recv().await;
                    tracing::info!("HTTP server graceful shutdown initiated");
                })
                .await
                .unwrap_or_else(|e| panic!("HTTP server error: {}", e));
            tracing::info!("HTTP server stopped");
        });

        Ok(Some(handle))
    }
}

// ---- response ---------------------------------------------------------------

use axum::response::IntoResponse;
use hygiea_core::{FmtErr, RawErr};
use serde::Serialize;

#[derive(Serialize)]
pub struct AxumHttpResponse<T: Serialize> {
    code: &'static str,
    msg: String,
    data: Option<T>,
}

impl<T: Serialize> AxumHttpResponse<T> {
    pub fn ok(data: T) -> Self {
        let e = hygiea::SuccessRawErr::Success.to_err();
        Self {
            code: e.err_code,
            msg: e.err_msg.to_string(),
            data: Some(data),
        }
    }
}

impl IntoResponse for AxumHttpResponse<()> {
    fn into_response(self) -> axum::response::Response {
        axum::response::Json(self).into_response()
    }
}

impl<T: Serialize> From<T> for AxumHttpResponse<T> {
    fn from(data: T) -> Self {
        Self::ok(data)
    }
}

pub enum AxumHttpError {
    Raw(RawErr),
    Fmt(FmtErr),
}

impl IntoResponse for AxumHttpError {
    fn into_response(self) -> axum::response::Response {
        let (code, msg) = match &self {
            AxumHttpError::Raw(e) => (e.err_code, e.to_string()),
            AxumHttpError::Fmt(e) => (e.err_code, e.to_string()),
        };
        axum::response::Json(AxumHttpResponse::<()> {
            code,
            msg,
            data: None,
        })
        .into_response()
    }
}

impl From<RawErr> for AxumHttpError {
    fn from(e: RawErr) -> Self {
        Self::Raw(e)
    }
}

impl From<FmtErr> for AxumHttpError {
    fn from(e: FmtErr) -> Self {
        Self::Fmt(e)
    }
}

impl From<anyhow::Error> for AxumHttpError {
    fn from(e: anyhow::Error) -> Self {
        e.downcast::<RawErr>()
            .map(|e| Self::Raw(e.into()))
            .or_else(|e| e.downcast::<FmtErr>().map(|e| Self::Fmt(e.into())))
            .unwrap_or_else(|e| {
                Self::Fmt(
                    hygiea::BaseFmtErr::SysFmtErr
                        .to_err(serde_json::json!({ "cause": e.to_string() })),
                )
            })
    }
}

// ---- handler adapters -------------------------------------------------------

/// Wraps a zero-argument async handler `() -> Result<R, E>` into an axum handler.
pub fn arity0<F, Fut, R, E>(
    f: F,
) -> impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = axum::response::Response> + Send>>
+ Clone
where
    F: Fn() -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Result<R, E>> + Send + 'static,
    R: serde::Serialize + Send + 'static,
    E: Into<AxumHttpError> + Send + 'static,
{
    move || {
        let f = f.clone();
        Box::pin(async move {
            match f().await {
                Ok(resp) => axum::response::Json(AxumHttpResponse::ok(resp)).into_response(),
                Err(e) => e.into().into_response(),
            }
        })
    }
}

/// Wraps a one-argument async handler `(P) -> Result<R, E>` into an axum handler.
pub fn arity1<F, Fut, R, P, E>(
    f: F,
) -> impl Fn(
    axum::extract::Json<P>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = axum::response::Response> + Send>>
+ Clone
where
    F: Fn(P) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Result<R, E>> + Send + 'static,
    P: Send + 'static,
    R: serde::Serialize + Send + 'static,
    E: Into<AxumHttpError> + Send + 'static,
{
    move |axum::extract::Json(req)| {
        let f = f.clone();
        Box::pin(async move {
            match f(req).await {
                Ok(resp) => axum::response::Json(AxumHttpResponse::ok(resp)).into_response(),
                Err(e) => e.into().into_response(),
            }
        })
    }
}

// ---- private ----------------------------------------------------------------

fn apply_layers(router: axum::Router, config: &AxumConfig) -> axum::Router {
    let router = match config.limits.as_ref().and_then(|l| l.max_body_size) {
        Some(size) => {
            tracing::info!("HTTP max body size set to {} bytes", size);
            router.layer(tower_http::limit::RequestBodyLimitLayer::new(size))
        }
        None => router,
    };

    let router = match config.timeout.as_ref().and_then(|t| t.request_read_secs) {
        Some(secs) => {
            let duration = Duration::from_secs(secs);
            tracing::info!("HTTP request timeout set to {:?}", duration);
            router.layer(tower_http::timeout::TimeoutLayer::with_status_code(
                http::StatusCode::REQUEST_TIMEOUT,
                duration,
            ))
        }
        None => router,
    };

    router
}
