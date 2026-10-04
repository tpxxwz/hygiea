//! `build_router` 加的中间件、`arity0` / `arity1` 适配器的集成测试。
//!
//! 不占端口：直接用 `tower::ServiceExt::oneshot` 打公开的 `build_router` 构造出来的 router。

#![cfg(feature = "axum")]

use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use hygiea_core::{Result, err, hy_err};
use hygiea_http::{AxumConfig, HttpLimits, HttpTimeout, arity0, arity1, build_router};
use tower::ServiceExt;

/// 业务错误：项目前缀取默认的 000，用来验证失败响应的状态码和 body
#[derive(hy_err)]
enum TestErr {
    #[error(err_code = "00001", err_tpl = "boom: {{ reason }}")]
    Boom,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Echo {
    msg: String,
}

async fn ok0() -> Result<&'static str> {
    Ok("pong")
}

async fn fail0() -> Result<&'static str> {
    Err(err!(TestErr::Boom, "zero"))
}

async fn ok1(req: Echo) -> Result<Echo> {
    Ok(req)
}

async fn fail1(_req: Echo) -> Result<Echo> {
    Err(err!(TestErr::Boom, "one"))
}

async fn slow0() -> Result<&'static str> {
    tokio::time::sleep(Duration::from_secs(2)).await;
    Ok("too slow")
}

fn test_router() -> Router {
    Router::new()
        .route("/ok0", get(arity0(ok0)))
        .route("/fail0", get(arity0(fail0)))
        .route("/ok1", post(arity1(ok1)))
        .route("/fail1", post(arity1(fail1)))
        .route("/slow0", get(arity0(slow0)))
}

async fn body_json(resp: axum::response::Response) -> (http::StatusCode, serde_json::Value) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn arity0_success_returns_data() {
    let router = build_router(test_router(), &AxumConfig::default());
    let req = http::Request::builder()
        .uri("/ok0")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, v) = body_json(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(v["code"], hygiea_core::SUCCESS_CODE);
    assert_eq!(v["data"], "pong");
}

#[tokio::test]
async fn arity0_failure_is_200_with_error_code() {
    let router = build_router(test_router(), &AxumConfig::default());
    let req = http::Request::builder()
        .uri("/fail0")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, v) = body_json(router.oneshot(req).await.unwrap()).await;
    // D1：业务错误的 HTTP 状态码一律 200，错误码放在 body 里
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(v["code"], err!(TestErr::Boom, "zero").err_code());
    assert_eq!(v["msg"], "boom: zero");
}

#[tokio::test]
async fn arity1_success_echoes_body() {
    let router = build_router(test_router(), &AxumConfig::default());
    let req = http::Request::builder()
        .method("POST")
        .uri("/ok1")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"msg":"hi"}"#))
        .unwrap();
    let (status, v) = body_json(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(v["code"], hygiea_core::SUCCESS_CODE);
    assert_eq!(v["data"]["msg"], "hi");
}

#[tokio::test]
async fn arity1_failure_is_200_with_error_code() {
    let router = build_router(test_router(), &AxumConfig::default());
    let req = http::Request::builder()
        .method("POST")
        .uri("/fail1")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"msg":"hi"}"#))
        .unwrap();
    let (status, v) = body_json(router.oneshot(req).await.unwrap()).await;
    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(v["code"], err!(TestErr::Boom, "one").err_code());
    assert_eq!(v["msg"], "boom: one");
}

/// axum 的 `Json` extractor 在请求体不是合法 JSON 时，在进 handler 之前就短路返回，
/// 走的是 axum 自己的 rejection，不经过 `AxumHttpError`：实际是 400，body 是纯文本说明，不是我们的
/// `{code, msg, data}` 结构。这里按实际行为断言。
#[tokio::test]
async fn arity1_malformed_json_is_axum_rejection() {
    let router = build_router(test_router(), &AxumConfig::default());
    let req = http::Request::builder()
        .method("POST")
        .uri("/ok1")
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{not json"))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    // 不是 JSON，是 axum 的纯文本 rejection 消息
    assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_err());
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        text.contains("Failed to parse the request body as JSON"),
        "{text}"
    );
}

#[tokio::test]
async fn body_over_limit_is_413() {
    let config = AxumConfig {
        limits: Some(HttpLimits {
            max_body_size: Some(8),
            ..Default::default()
        }),
        ..Default::default()
    };
    let router = build_router(test_router(), &config);
    let req = http::Request::builder()
        .method("POST")
        .uri("/ok1")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            r#"{"msg":"way too long for the limit"}"#,
        ))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn handler_too_slow_is_408() {
    let config = AxumConfig {
        timeout: Some(HttpTimeout {
            request_read_secs: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    let router = build_router(test_router(), &config);
    let req = http::Request::builder()
        .uri("/slow0")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::REQUEST_TIMEOUT);
}
