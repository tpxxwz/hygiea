//! reqwest 用法速查：全部是对 reqwest 原生 API 的直接调用，不经过任何封装。
//! 需要联网，默认 #[ignore]，跑法：
//! cargo test -p hygiea-examples --test reqwest_api -- --ignored

use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

const HTTPBIN_BASE: &str = "https://httpbin.org";

#[tokio::test]
#[ignore]
async fn test_get_request() {
    let client = Client::builder().build().unwrap();

    let resp = client
        .get(format!("{HTTPBIN_BASE}/get"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore]
async fn test_get_with_query() {
    let client = Client::builder().build().unwrap();

    let resp = client
        .get(format!("{HTTPBIN_BASE}/get"))
        .query(&[("name", "alice"), ("age", "30")])
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    assert_eq!(resp["args"]["name"], "alice");
    assert_eq!(resp["args"]["age"], "30");
}

#[tokio::test]
#[ignore]
async fn test_get_with_headers() {
    let client = Client::builder().build().unwrap();

    let resp = client
        .get(format!("{HTTPBIN_BASE}/headers"))
        .header("X-Custom-Header", "hygiea")
        .bearer_auth("test-token")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    assert_eq!(resp["headers"]["X-Custom-Header"], "hygiea");
    assert_eq!(resp["headers"]["Authorization"], "Bearer test-token");
}

#[tokio::test]
#[ignore]
async fn test_post_json() {
    let client = Client::builder().build().unwrap();

    let body = serde_json::json!({"name": "bob", "age": 25});
    let resp = client
        .post(format!("{HTTPBIN_BASE}/post"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    assert_eq!(resp["json"], body);
}

#[tokio::test]
#[ignore]
async fn test_status_error() {
    let client = Client::builder().build().unwrap();

    let resp = client
        .get(format!("{HTTPBIN_BASE}/status/404"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // error_for_status_ref 只借用，判断完还能继续读 body；
    // error_for_status(self) 是按值消费，出错时 body 直接被丢弃，读不到了。
    assert!(resp.error_for_status_ref().is_err());

    let body = resp.text().await.unwrap();
    assert!(body.is_empty()); // httpbin /status/{code} 本身就不返回 body
}

#[tokio::test]
#[ignore]
async fn test_client_builder_config() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("X-App", reqwest::header::HeaderValue::from_static("hygiea"));

    let client = Client::builder()
        .timeout(Duration::from_millis(1))
        .connect_timeout(Duration::from_secs(5))
        .user_agent("hygiea/0.1")
        .default_headers(headers)
        .pool_max_idle_per_host(4)
        .build()
        .unwrap();

    let err = client
        .get(format!("{HTTPBIN_BASE}/delay/3"))
        .send()
        .await
        .unwrap_err();

    assert!(err.is_timeout());
}

#[tokio::test]
#[ignore]
async fn test_request_level_timeout_overrides_client_default() {
    // client 级别的默认 timeout 给得很宽松，单独这一次请求不该超时。
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let ok = client
        .get(format!("{HTTPBIN_BASE}/delay/1"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    // 单次请求用 .timeout() 覆盖成极短，优先级比 client 默认值高，照样会超时。
    let err = client
        .get(format!("{HTTPBIN_BASE}/delay/3"))
        .timeout(Duration::from_millis(1))
        .send()
        .await
        .unwrap_err();

    assert!(err.is_timeout());
}

#[tokio::test]
#[ignore]
async fn test_multipart_form_upload() {
    let client = Client::builder().build().unwrap();

    let form = reqwest::multipart::Form::new()
        .text("field1", "value1")
        .part(
            "file",
            reqwest::multipart::Part::bytes(b"hello world".to_vec())
                .file_name("test.txt")
                .mime_str("text/plain")
                .unwrap(),
        );

    let resp = client
        .post(format!("{HTTPBIN_BASE}/post"))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    assert_eq!(resp["form"]["field1"], "value1");
    assert_eq!(resp["files"]["file"], "hello world");
}

#[tokio::test]
#[ignore]
async fn test_stream_upload_from_file() {
    let path = std::env::temp_dir().join("hygiea_test_upload.txt");
    fs::write(&path, b"streamed file content").await.unwrap();

    let file = fs::File::open(&path).await.unwrap();
    let client = Client::builder().build().unwrap();

    let resp = client
        .post(format!("{HTTPBIN_BASE}/post"))
        .body(file)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    assert_eq!(resp["data"], "streamed file content");

    fs::remove_file(&path).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_wrap_stream_upload() {
    let path = std::env::temp_dir().join("hygiea_test_wrap_stream_upload.txt");
    fs::write(&path, b"wrapped stream content").await.unwrap();

    let file = fs::File::open(&path).await.unwrap();
    let body = reqwest::Body::wrap_stream(ReaderStream::new(file));

    let client = Client::builder().build().unwrap();
    let resp = client
        .post(format!("{HTTPBIN_BASE}/post"))
        .body(body)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    assert_eq!(resp["data"], "wrapped stream content");

    fs::remove_file(&path).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_chunked_download() {
    let client = Client::builder().build().unwrap();
    let path = std::env::temp_dir().join("hygiea_test_chunked_download.bin");

    let mut resp = client
        .get(format!("{HTTPBIN_BASE}/bytes/1024"))
        .send()
        .await
        .unwrap();

    let mut file = fs::File::create(&path).await.unwrap();
    let mut total = 0usize;
    while let Some(chunk) = resp.chunk().await.unwrap() {
        total += chunk.len();
        file.write_all(&chunk).await.unwrap();
    }

    assert_eq!(total, 1024);
    fs::remove_file(&path).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn test_bytes_stream_download() {
    let client = Client::builder().build().unwrap();

    let resp = client
        .get(format!("{HTTPBIN_BASE}/bytes/2048"))
        .send()
        .await
        .unwrap();

    let mut stream = resp.bytes_stream();
    let mut total = 0usize;
    while let Some(chunk) = stream.next().await {
        total += chunk.unwrap().len();
    }

    assert_eq!(total, 2048);
}

// ===== Proxy =====
// 下面前两个不联网，只验证 Proxy 的构造和匹配规则，正常 cargo test 就会跑；
// 真正走代理的那两个需要本地起代理，保持 #[ignore]。

/// `http` / `https` / `all` 决定这个代理对哪些**目标 URL 的 scheme** 生效，
/// 跟代理服务器自身用什么协议无关——代理地址写 `http://` 但拦的是 https 请求，是正常配置。
#[test]
fn test_proxy_scheme_variants() {
    // 只代理 http:// 的目标
    let _http_only = reqwest::Proxy::http("http://127.0.0.1:7890").unwrap();
    // 只代理 https:// 的目标
    let _https_only = reqwest::Proxy::https("http://127.0.0.1:7890").unwrap();
    // 两者都代理
    let all = reqwest::Proxy::all("http://127.0.0.1:7890").unwrap();

    // 代理需要认证时，会拼成 Proxy-Authorization 头
    let _with_auth = reqwest::Proxy::all("http://127.0.0.1:7890")
        .unwrap()
        .basic_auth("user", "pass");

    // socks5h 的 h 表示域名交给代理去解析，本地不做 DNS；socks5 则是本地解析后再把 IP 给代理。
    // 爬墙或者内网穿透时通常要 socks5h，否则 DNS 会在本地失败或者被污染
    let _socks = reqwest::Proxy::all("socks5h://127.0.0.1:1080").unwrap();

    // custom 按目标 URL 动态决定走哪个代理，返回 None 表示这次直连
    let _custom = reqwest::Proxy::custom(|url| {
        if url.host_str() == Some("example.com") {
            Some("http://127.0.0.1:7890")
        } else {
            None
        }
    });

    // 多个 proxy 按加入顺序匹配，第一个命中的生效
    let client = Client::builder().proxy(all).build().unwrap();
    drop(client);
}

/// `NoProxy` 是排除名单：命中的目标绕过代理直连。
/// 支持域名后缀、IP、CIDR 网段，逗号分隔
#[test]
fn test_proxy_no_proxy_exclusion() {
    let no_proxy =
        reqwest::NoProxy::from_string("localhost,127.0.0.1,192.168.0.0/16,.internal.com");

    let proxy = reqwest::Proxy::all("http://127.0.0.1:7890")
        .unwrap()
        .no_proxy(no_proxy);

    let client = Client::builder().proxy(proxy).build().unwrap();
    drop(client);

    // 也可以从 NO_PROXY / no_proxy 环境变量读，没设则是 None
    let _from_env = reqwest::NoProxy::from_env();
}

/// 真的把请求发出去。需要本地 7890 上有 HTTP 代理（Clash/mihomo 默认端口）
#[tokio::test]
#[ignore]
async fn test_proxy_request() {
    let proxy = reqwest::Proxy::all("http://127.0.0.1:7890").unwrap();
    let client = Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    // httpbin 回显它看到的来源 IP，走代理的话这里是代理的出口 IP
    let resp = client
        .get(format!("{HTTPBIN_BASE}/ip"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    println!("[via proxy] origin = {}", resp["origin"]);
    assert!(resp["origin"].is_string());
}

/// `no_proxy()` 关掉一切代理，连环境变量里的 HTTP_PROXY 也不认
#[tokio::test]
#[ignore]
async fn test_no_proxy_overrides_env() {
    let client = Client::builder().no_proxy().build().unwrap();

    let resp = client
        .get(format!("{HTTPBIN_BASE}/ip"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}
