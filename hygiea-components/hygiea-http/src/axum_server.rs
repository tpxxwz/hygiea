//! HTTP 服务组件（axum），feature `axum`

use std::time::Duration;

use serde::Deserialize;

use hygiea_core::app::{
    BaseAppErr, CancellationToken, DeferredComponent, Name, ReadyResources, ResourceSink, component,
};
use hygiea_core::{HyErr, ResultExt, err};

// ---- config -----------------------------------------------------------------

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, deny_unknown_fields)]
pub struct HttpTimeout {
    pub connect_secs: Option<u64>,
    pub header_read_secs: Option<u64>,
    pub request_read_secs: Option<u64>,
    pub response_write_secs: Option<u64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, deny_unknown_fields)]
pub struct HttpLimits {
    pub max_body_size: Option<usize>,
    pub max_header_size: Option<usize>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct AxumConfig {
    pub host: String,
    pub port: u16,
    pub base_path: String,
    pub timeout: Option<HttpTimeout>,
    pub limits: Option<HttpLimits>,
    /// 关闭时最多等多久（秒）让手上的请求处理完，不写用 Registry 的默认值（15 秒）。
    /// 有长请求（大文件上传、长轮询）就配长一点
    pub shutdown_timeout_secs: Option<u64>,
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
            shutdown_timeout_secs: None,
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

/// HTTP 服务组件，Deferred：`prepare` 检查配置、绑定端口，`activate` 才开始接请求
pub struct AxumComponent {
    config: AxumConfig,
    /// `prepare` 绑好的端口和取出的 router，`activate` 里交给后台任务
    prepared: Option<(tokio::net::TcpListener, axum::Router)>,
}

#[component]
impl DeferredComponent for AxumComponent {
    type Config = AxumConfig;

    fn build(_name: Name, config: Self::Config) -> Self {
        Self {
            config,
            prepared: None,
        }
    }

    fn shutdown_timeout(&self) -> Option<Duration> {
        self.config.shutdown_timeout_secs.map(Duration::from_secs)
    }

    async fn prepare(&mut self, _sink: &ResourceSink) -> Result<(), HyErr> {
        let addr = self.config.addr();
        let router = self
            .config
            .router
            .take()
            .ok_or_else(|| err!(BaseAppErr::ConfigMissing, "AxumConfig.router"))?;

        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .wrap_err(|| err!(BaseAppErr::BindFailed, &addr))?;

        tracing::info!("HTTP server listener bound to {}", addr);
        self.prepared = Some((listener, router));
        Ok(())
    }

    async fn activate(
        &mut self,
        _resources: ReadyResources,
        shutdown: CancellationToken,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, HyErr> {
        let (listener, router) = self.prepared.take().ok_or_else(|| {
            err!(
                BaseAppErr::ComponentError,
                "AxumComponent activated before prepare"
            )
        })?;
        let addr = self.config.addr();
        let config = self.config.clone();

        let handle = tokio::spawn(async move {
            tracing::info!("HTTP server serving on {}", addr);

            let router = build_router(router, &config);

            let served = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    shutdown.cancelled().await;
                    tracing::info!("HTTP server graceful shutdown initiated");
                })
                .await;
            match served {
                Ok(()) => tracing::info!("HTTP server stopped"),
                Err(e) => tracing::error!("HTTP server error: {e}"),
            }
        });

        Ok(Some(handle))
    }
}

// ---- response ---------------------------------------------------------------

use axum::response::IntoResponse;
use serde::Serialize;

#[derive(Serialize)]
pub struct AxumHttpResponse<T: Serialize> {
    code: &'static str,
    msg: String,
    data: Option<T>,
}

impl<T: Serialize> AxumHttpResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            code: hygiea_core::SUCCESS_CODE,
            msg: String::new(),
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

/// 包一层是因为 `HyErr` 和 `IntoResponse` 都是外部类型，孤儿规则不让直接 impl
pub struct AxumHttpError(HyErr);

impl IntoResponse for AxumHttpError {
    /// HTTP 状态码一律 200，错误放在 body 的 `code` / `msg` 里。
    ///
    /// - 框架内置错误（项目前缀 [`FRAMEWORK_ERR_PREFIX`]）：框架不知道模板参数里哪些能给客户端看，
    ///   对外统一换成 `SysErr`（"System Error"），原错误码和 `{:#}` 整条链只打在服务端日志里。
    ///   要给客户端看什么，由业务转成自己前缀的错误
    /// - 业务错误：客户端拿到 `Display` 的那句对外消息；挂了 source 的（内部故障）在服务端按 `{:#}`
    ///   打出整条链，没有 source 的属于正常分支，不打日志
    fn into_response(self) -> axum::response::Response {
        let e = self.0;
        let (code, msg) = if e.err_code().starts_with(FRAMEWORK_ERR_PREFIX) {
            tracing::error!(code = e.err_code(), "{e:#}");
            let sys = hygiea_core::err!(hygiea_core::BaseErr::SysErr);
            (sys.err_code(), sys.to_string())
        } else {
            if std::error::Error::source(&e).is_some() {
                tracing::error!(code = e.err_code(), "{e:#}");
            }
            (e.err_code(), e.to_string())
        };
        axum::response::Json(AxumHttpResponse::<()> {
            code,
            msg,
            data: None,
        })
        .into_response()
    }
}

/// 框架内置错误的项目前缀，对外不展示原消息，见 [`AxumHttpError`] 的 `into_response`
pub const FRAMEWORK_ERR_PREFIX: &str = "999";

impl From<HyErr> for AxumHttpError {
    fn from(e: HyErr) -> Self {
        Self(e)
    }
}

impl From<anyhow::Error> for AxumHttpError {
    fn from(e: anyhow::Error) -> Self {
        // 不认识的错误对外只说 "System Error"，原错误挂 source，由 into_response 记日志
        Self(
            e.downcast::<HyErr>()
                .unwrap_or_else(|e| hygiea_core::err!(hygiea_core::BaseErr::SysErr).with_source(e)),
        )
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

// ---- router 构造 --------------------------------------------------------------

/// 给业务的 router 加上超时、body 大小限制等中间件，`activate` 用它拼出最终对外服务的 router。
///
/// 公开出来是为了让集成测试能不占端口、直接用 `tower::ServiceExt::oneshot` 打这个 router，
/// 验证 413 / 408 等中间件行为；`AxumConfig` 里没配的项不加对应的 layer，行为和不配一样。
pub fn build_router(router: axum::Router, config: &AxumConfig) -> axum::Router {
    let router = match config.limits.as_ref().and_then(|l| l.max_body_size) {
        Some(size) => {
            tracing::info!("HTTP max body size set to {} bytes", size);
            router.layer(tower_http::limit::RequestBodyLimitLayer::new(size))
        }
        None => router,
    };

    match config.timeout.as_ref().and_then(|t| t.request_read_secs) {
        Some(secs) => {
            let duration = Duration::from_secs(secs);
            tracing::info!("HTTP request timeout set to {:?}", duration);
            router.layer(tower_http::timeout::TimeoutLayer::with_status_code(
                http::StatusCode::REQUEST_TIMEOUT,
                duration,
            ))
        }
        None => router,
    }
}

#[cfg(test)]
mod tests {
    use hygiea_core::{BaseErr, err, hy_err};

    use super::*;

    /// 业务错误：项目前缀取默认的 000，不是框架的 999
    #[derive(hy_err)]
    enum BizErr {
        #[error(err_code = "00001", err_tpl = "User {{ name }} not found")]
        UserNotFound,
    }

    async fn body(err: AxumHttpError) -> (http::StatusCode, serde_json::Value) {
        let resp = err.into_response();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn business_error_keeps_code_and_message() {
        let e = err!(BizErr::UserNotFound, "Alice");
        let code = e.err_code();
        let (status, v) = body(e.into()).await;
        assert_eq!(status, http::StatusCode::OK);
        assert_eq!(v["code"], code);
        assert_eq!(v["msg"], "User Alice not found");
        assert!(v["data"].is_null());
    }

    #[tokio::test]
    async fn framework_error_is_hidden_as_sys_err() {
        // 框架内置错误的模板参数可能带内部信息，对外只给 SysErr
        let e = err!(BaseErr::RegexError, "secret-internal-pattern");
        assert!(e.err_code().starts_with(FRAMEWORK_ERR_PREFIX));
        let (status, v) = body(e.into()).await;
        let sys = err!(BaseErr::SysErr);
        assert_eq!(status, http::StatusCode::OK);
        assert_eq!(v["code"], sys.err_code());
        assert_eq!(v["msg"], "System Error");
        assert!(!v.to_string().contains("secret-internal-pattern"));
    }

    #[tokio::test]
    async fn unknown_error_becomes_sys_err() {
        let e: AxumHttpError = anyhow::anyhow!("db password leaked").into();
        let (_, v) = body(e).await;
        assert_eq!(v["code"], err!(BaseErr::SysErr).err_code());
        assert!(!v.to_string().contains("password"));
    }

    #[tokio::test]
    async fn hy_err_inside_anyhow_is_recovered() {
        let e: AxumHttpError = anyhow::Error::new(err!(BizErr::UserNotFound, "Bob")).into();
        let (_, v) = body(e).await;
        assert_eq!(v["msg"], "User Bob not found");
    }

    #[tokio::test]
    async fn success_response_has_code_msg_data() {
        let resp = axum::response::Json(AxumHttpResponse::ok(42)).into_response();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["code"], hygiea_core::SUCCESS_CODE);
        assert_eq!(v["msg"], "");
        assert_eq!(v["data"], 42);
    }

    #[test]
    fn addr_joins_host_and_port() {
        let config = AxumConfig {
            host: "127.0.0.1".to_string(),
            port: 9090,
            ..Default::default()
        };
        assert_eq!(config.addr(), "127.0.0.1:9090");
    }

    #[test]
    fn axum_config_default_values() {
        let config = AxumConfig::default();
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.port, 8080);
        assert_eq!(config.base_path, "");
        assert!(config.timeout.is_none());
        assert!(config.limits.is_none());
        assert!(config.shutdown_timeout_secs.is_none());
        assert!(config.router.is_none());
    }

    #[test]
    fn axum_config_deserialize_overrides_given_fields() {
        let config: AxumConfig = serde_json::from_str(
            r#"{
                "host": "127.0.0.1",
                "port": 9090,
                "base_path": "/api",
                "timeout": {"request_read_secs": 5},
                "limits": {"max_body_size": 1024},
                "shutdown_timeout_secs": 10
            }"#,
        )
        .unwrap();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 9090);
        assert_eq!(config.base_path, "/api");
        assert_eq!(config.timeout.unwrap().request_read_secs, Some(5));
        assert_eq!(config.limits.unwrap().max_body_size, Some(1024));
        assert_eq!(config.shutdown_timeout_secs, Some(10));
    }

    #[test]
    fn axum_config_unknown_field_errors() {
        let result: Result<AxumConfig, _> = serde_json::from_str(r#"{"nope": 1}"#);
        assert!(result.is_err());
    }

    #[test]
    fn http_timeout_default_values() {
        let timeout = HttpTimeout::default();
        assert!(timeout.connect_secs.is_none());
        assert!(timeout.header_read_secs.is_none());
        assert!(timeout.request_read_secs.is_none());
        assert!(timeout.response_write_secs.is_none());
    }

    #[test]
    fn http_timeout_deserialize_overrides_given_fields() {
        let timeout: HttpTimeout =
            serde_json::from_str(r#"{"connect_secs": 3, "request_read_secs": 7}"#).unwrap();
        assert_eq!(timeout.connect_secs, Some(3));
        assert_eq!(timeout.header_read_secs, None);
        assert_eq!(timeout.request_read_secs, Some(7));
        assert_eq!(timeout.response_write_secs, None);
    }

    #[test]
    fn http_timeout_unknown_field_errors() {
        let result: Result<HttpTimeout, _> = serde_json::from_str(r#"{"nope": 1}"#);
        assert!(result.is_err());
    }

    #[test]
    fn http_limits_default_values() {
        let limits = HttpLimits::default();
        assert!(limits.max_body_size.is_none());
        assert!(limits.max_header_size.is_none());
    }

    #[test]
    fn http_limits_deserialize_overrides_given_fields() {
        let limits: HttpLimits = serde_json::from_str(r#"{"max_body_size": 2048}"#).unwrap();
        assert_eq!(limits.max_body_size, Some(2048));
        assert_eq!(limits.max_header_size, None);
    }

    #[test]
    fn http_limits_unknown_field_errors() {
        let result: Result<HttpLimits, _> = serde_json::from_str(r#"{"nope": 1}"#);
        assert!(result.is_err());
    }
}
