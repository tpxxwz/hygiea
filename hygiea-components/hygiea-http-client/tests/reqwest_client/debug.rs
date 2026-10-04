//! debug 日志（`debug-log` feature）：打开后每次请求打两条 JSON——start 是请求，结束是请求加响应，
//! url、params、body、resp 是原文，带请求头和响应头，无视 enable_logging；日志里的请求头和服务端实际收到的一致。
//!
//! 运行：`cargo test -p hygiea-http-client --features reqwest,debug-log --test reqwest_client debug::`

use hygiea_http_client::reqwest_client::*;
use serde_json::{Value, json};

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

/// 日志里 `message` 后面的那段 JSON（紧凑时隔一个空格，pretty 时隔一个换行）
fn debug_json(log: &str, message: &str) -> Value {
    let at = [" {", "\n{"]
        .iter()
        .find_map(|sep| log.find(&format!("{message}{sep}")))
        .unwrap_or_else(|| panic!("no `{message}` in:\n{log}"));
    serde_json::Deserializer::from_str(&log[at + message.len()..])
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap()
}

/// 成功：start 和 success 都打（enable_logging 关了也打）；success 里有完整的请求和响应，都是原文
#[tokio::test]
async fn success_has_request_and_response() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    login_request(format!("{base}/echo"))
        .send::<Json<Value>>(&debug_client())
        .await
        .unwrap();
    let log = out.text();
    let start = debug_json(&log, "http call start");
    let success = debug_json(&log, "http call success");
    let login = json!({"user": "alice", "token": "t0k"});
    for v in [&start, &success] {
        assert_eq!(v["method"], "POST");
        assert_eq!(v["url"], format!("{base}/echo?user=alice&token=t0k"));
        assert_eq!(v["request"]["params"], login);
        assert_eq!(v["request"]["body"], login);
    }
    assert_eq!(success["request"], start["request"]);
    assert_eq!(success["status"], 200);
    assert_eq!(success["error"], Value::Null);
    // response.body 是回显的原文，嵌套成 JSON；里面是服务端收到的 body 和 query
    let resp = &success["response"];
    assert_eq!(resp["headers"]["content-type"], "application/json");
    assert_eq!(resp["body"]["query"], "user=alice&token=t0k");
    assert_eq!(resp["body"]["body"], r#"{"user":"alice","token":"t0k"}"#);
}

/// 日志里的请求头和服务端实际收到的一致：请求自己的头、Json 加的 content-type、client 的 UA / 默认头 / Accept。
/// 照抄的 reqwest 默认头规则变了时这里会失败。
/// 发送时才加的 host、content-length、accept-encoding（透明压缩）不在日志里
#[tokio::test]
async fn req_headers_match_what_server_received() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    let seen = login_request(format!("{base}/echo"))
        .send::<Json<Value>>(&debug_client())
        .await
        .unwrap()
        .body;
    let logged = debug_json(&out.text(), "http call start")["request"]["headers"].clone();
    let mut received = seen["headers"].as_object().unwrap().clone();
    for added_when_sending in ["host", "content-length", "accept-encoding"] {
        received.remove(added_when_sending);
    }
    assert_eq!(logged, Value::Object(received));
}

/// 带重试的路径（body 读完再解码）同样有响应原文
#[tokio::test]
async fn retry_path_has_response_body() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    login_request(format!("{base}/echo"))
        .send::<Json<Value>>((&debug_client(), RetryCtx::new(0, NoRetry)))
        .await
        .unwrap();
    let success = debug_json(&out.text(), "http call success");
    assert_eq!(success["response"]["body"]["query"], "user=alice&token=t0k");
}

/// 解码成打码类型时，平时 resp 是打码后的，debug 是 body 原文
#[tokio::test]
async fn masked_decoder_logs_raw_body() {
    let base = serve_routes(&[("/login", 200, r#"{"user":"alice","token":"t0k"}"#)]).await;
    let (out, _guard) = capture();
    RequestConfig::plain(Method::GET, format!("{base}/login"))
        .send::<Json<Login>>(&debug_client())
        .await
        .unwrap();
    let success = debug_json(&out.text(), "http call success");
    assert_eq!(
        success["response"]["body"],
        json!({"user": "alice", "token": "t0k"})
    );
}

/// 非 2xx：按失败级别打，有状态码和响应，没有 error
#[tokio::test]
async fn non_2xx_has_response() {
    let base = serve_routes(&[("/fail", 503, r#"{"err":"down"}"#)]).await;
    let (out, _guard) = capture();
    RequestConfig::plain(Method::GET, format!("{base}/fail"))
        .send::<Bytes>(&debug_client())
        .await
        .unwrap_err();
    let log = out.text();
    assert!(log.contains(" WARN "), "{log}");
    let failed = debug_json(&log, "http call non-2xx");
    assert_eq!(failed["status"], 503);
    assert_eq!(failed["response"]["body"], json!({"err": "down"}));
    assert_eq!(failed["error"], Value::Null);
}

/// 解码失败：有响应原文，error 是解码错误
#[tokio::test]
async fn decode_failed_has_response_and_error() {
    let base = serve_routes(&[("/bad", 200, r#"{"user":1}"#)]).await;
    let (out, _guard) = capture();
    RequestConfig::plain(Method::GET, format!("{base}/bad"))
        .send::<Json<Login>>(&debug_client())
        .await
        .unwrap_err();
    let failed = debug_json(&out.text(), "http call decode failed");
    assert_eq!(failed["status"], 200);
    assert_eq!(failed["response"]["body"], json!({"user": 1}));
    assert!(failed["error"].is_string(), "{failed:#}");
}

/// BodyStream 不读 body：成功日志有响应头，response.body 是 null
#[tokio::test]
async fn body_stream_has_headers_but_no_body() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    RequestConfig::plain(Method::GET, format!("{base}/echo"))
        .send::<BodyStream>(&debug_client())
        .await
        .unwrap();
    let success = debug_json(&out.text(), "http call success");
    assert!(success["response"]["headers"].is_object(), "{success:#}");
    assert_eq!(success["response"]["body"], Value::Null);
}

/// 默认每条一行；set_pretty 后缩进成多行，内容一样。从 Resources 拿到的 clone 也一起变
#[tokio::test]
async fn pretty_switch() {
    let base = serve(echo).await;
    let client = debug_client();
    let send = |client: ReqwestClient| {
        let url = format!("{base}/echo");
        async move {
            let (out, _guard) = capture();
            RequestConfig::plain(Method::GET, url)
                .send::<Bytes>(&client)
                .await
                .unwrap();
            out.text()
        }
    };
    let compact = send(client.clone()).await;
    assert_eq!(compact.lines().count(), 2, "{compact}");
    client.set_pretty(true);
    let pretty = send(client.clone()).await;
    assert!(pretty.lines().count() > 10, "{pretty}");
    let mut a = debug_json(&compact, "http call success");
    let mut b = debug_json(&pretty, "http call success");
    for v in [&mut a, &mut b] {
        // 耗时和响应日期每次不同
        v["elapsed_ms"] = Value::Null;
        v["response"]["headers"]["date"] = Value::Null;
    }
    assert_eq!(a, b);
}

/// debug 关时还是原来的单行日志，打码照旧
#[tokio::test]
async fn off_keeps_original_logs() {
    let base = serve(echo).await;
    let (out, _guard) = capture();
    RequestConfig::with_params(Method::GET, format!("{base}/echo"), login("t0k"))
        .send::<Bytes>(&local_config().build().unwrap())
        .await
        .unwrap();
    let log = out.text();
    assert_eq!(log.lines().count(), 2, "{log}");
    assert!(log.contains("http call start method=GET"), "{log}");
    assert!(!log.contains("t0k"), "{log}");
}
