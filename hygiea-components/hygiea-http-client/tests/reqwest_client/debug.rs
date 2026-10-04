//! debug 日志（`debug-log` feature）：打开后 url、req、resp 是原文，带请求头和响应头，
//! 无视 enable_logging；日志里的请求头和服务端实际收到的一致。
//!
//! 运行：`cargo test -p hygiea-http-client --features reqwest,debug-log --test reqwest_client debug::`

use hygiea_http_client::reqwest_client::*;

use crate::support::*;

/// 开了 debug 的 client，带 UA 和一个默认头
fn debug_client() -> ReqwestClient {
    ReqwestConfig {
        user_agent: Some("hygiea-debug".into()),
        ..local_config()
    }
    .default_headers(header_map(&[("x-default", "d")]))
    .unwrap()
    .debug(true)
    .build()
    .unwrap()
}

/// 发一个带敏感字段的 POST：params 和 body 都是 login，单请求头一个，关掉 enable_logging
fn login_request(url: String) -> RequestConfig<Login, Json<Login>> {
    RequestConfig::new(Method::POST, url, login("t0k"), Json(login("t0k")))
        .headers(header_map(&[("x-req", "r")]))
        .unwrap()
        .enable_logging(false)
}

/// 只发一次、不重试的策略（max_retries = 0 时不会被调用）
struct NoRetry;

impl<Params: Send, Req: Send> Retry<Params, Req> for NoRetry {
    async fn retry(
        &mut self,
        _attempt: usize,
        _cfg: RequestConfig<Params, Req>,
        failure: SendFailure,
    ) -> RetryDecision<Params, Req> {
        RetryDecision::Stop(failure.err)
    }
}

/// 日志里的这一行（按消息找）
fn line<'a>(log: &'a str, message: &str) -> &'a str {
    log.lines()
        .find(|l| l.contains(message))
        .unwrap_or_else(|| panic!("no `{message}` in:\n{log}"))
}

/// 成功：start / success 都打（enable_logging 关了也打），url、req、resp 是原文，带两种头
#[tokio::test]
async fn success_logs_raw_content_and_headers() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    login_request(format!("{base}/echo"))
        .send::<Json<serde_json::Value>>(&debug_client())
        .await
        .unwrap();
    let log = out.text();
    assert_eq!(log.lines().count(), 2, "{log}");
    for l in [
        line(&log, "http call start"),
        line(&log, "http call success"),
    ] {
        assert!(l.contains("/echo?user=alice&token=t0k"), "{l}");
        assert!(
            l.contains(r#"req=[params:{"user":"alice","token":"t0k"}, body:{"user":"alice","token":"t0k"}]"#),
            "{l}"
        );
        assert!(l.contains("req_headers={"), "{l}");
        assert!(!l.contains("***"), "{l}");
    }
    let success = line(&log, "http call success");
    // resp 是回显的原文（键按字母排序），里面有服务端收到的 body 和 query
    assert!(success.contains(r#"resp={"body":"#), "{success}");
    assert!(
        success.contains(r#""query":"user=alice&token=t0k""#),
        "{success}"
    );
    assert!(success.contains("resp_headers={"), "{success}");
    assert!(
        success.contains("content-type: application/json"),
        "{success}"
    );
}

/// 日志里的请求头和服务端实际收到的一致：请求自己的头、Json 加的 content-type、client 的 UA / 默认头 / Accept。
/// 照抄的 reqwest 默认头规则变了时这里会失败。
/// 发送时才加的 host、content-length、accept-encoding（透明压缩）不在日志里
#[tokio::test]
async fn req_headers_match_what_server_received() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    let seen = login_request(format!("{base}/echo"))
        .send::<Json<serde_json::Value>>(&debug_client())
        .await
        .unwrap()
        .body;
    let log = out.text();
    let start = line(&log, "http call start");
    let received = seen["headers"].as_object().unwrap();
    for (name, value) in received {
        if matches!(name.as_str(), "host" | "content-length" | "accept-encoding") {
            continue;
        }
        let part = format!("{name}: {}", value.as_str().unwrap());
        assert!(start.contains(&part), "{part} missing:\n{start}");
    }
    for name in ["x-req", "x-default", "user-agent", "accept", "content-type"] {
        assert!(
            received.contains_key(name),
            "server did not get {name}: {seen}"
        );
    }
}

/// 带重试的路径（body 读完再解码）同样打 resp 原文和头
#[tokio::test]
async fn retry_path_logs_raw_resp() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    login_request(format!("{base}/echo"))
        .send::<Json<serde_json::Value>>((&debug_client(), RetryCtx::new(0, NoRetry)))
        .await
        .unwrap();
    let log = out.text();
    let success = line(&log, "http call success");
    assert!(success.contains(r#"resp={"body":"#), "{success}");
    assert!(success.contains("resp_headers={"), "{success}");
}

/// 解码成打码类型时，平时 resp 是打码后的，debug 打 body 原文
#[tokio::test]
async fn masked_decoder_logs_raw_body() {
    let base = serve_routes(&[("/login", 200, r#"{"user":"alice","token":"t0k"}"#)]).await;
    let (out, _guard) = capture();
    RequestConfig::plain(Method::GET, format!("{base}/login"))
        .send::<Json<Login>>(&debug_client())
        .await
        .unwrap();
    let log = out.text();
    let success = line(&log, "http call success");
    assert!(
        success.contains(r#"resp={"user":"alice","token":"t0k"}"#),
        "{success}"
    );
}

/// 非 2xx：失败日志带响应头，resp 本来就是原文
#[tokio::test]
async fn non_2xx_logs_resp_headers() {
    let base = serve_routes(&[("/fail", 503, r#"{"err":"down"}"#)]).await;
    let (out, _guard) = capture();
    RequestConfig::plain(Method::GET, format!("{base}/fail"))
        .send::<Bytes>(&debug_client())
        .await
        .unwrap_err();
    let log = out.text();
    let failed = line(&log, "http call non-2xx");
    assert!(failed.contains(r#"resp={"err":"down"}"#), "{failed}");
    assert!(failed.contains("req_headers={"), "{failed}");
    assert!(failed.contains("resp_headers={"), "{failed}");
}

/// BodyStream 不读 body：成功日志只有响应头，没有 resp
#[tokio::test]
async fn body_stream_has_headers_but_no_resp() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    RequestConfig::plain(Method::GET, format!("{base}/echo"))
        .send::<BodyStream>(&debug_client())
        .await
        .unwrap();
    let log = out.text();
    let success = line(&log, "http call success");
    assert!(success.contains("resp_headers={"), "{success}");
    assert!(!success.contains("resp="), "{success}");
}

/// debug 关时没有头字段，打码照旧
#[tokio::test]
async fn off_has_no_headers() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    RequestConfig::with_params(Method::GET, format!("{base}/echo"), login("t0k"))
        .send::<Bytes>(&local_config().build().unwrap())
        .await
        .unwrap();
    let log = out.text();
    for l in log.lines() {
        assert!(!l.contains("req_headers="), "{l}");
        assert!(!l.contains("resp_headers="), "{l}");
        assert!(!l.contains("t0k"), "{l}");
    }
}
