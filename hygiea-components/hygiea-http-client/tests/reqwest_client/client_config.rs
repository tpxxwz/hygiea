//! `ReqwestConfig` 的各个选项在真实请求上的效果

use std::error::Error as _;
use std::net::SocketAddr;
use std::time::Duration;

use hygiea_http_client::reqwest_client::*;

use crate::support::*;

/// 发一个 GET 到回显服务，拿回服务端看到的请求
async fn echo_via(client: &Client, url: String) -> serde_json::Value {
    RequestConfig::plain(Method::GET, url)
        .send::<Json<serde_json::Value>>(client)
        .await
        .unwrap()
        .body
}

/// 请求头相关：User-Agent、默认头、透明压缩
mod headers {
    use super::*;

    /// user_agent 会带在每个请求上
    #[tokio::test]
    async fn user_agent_is_sent() {
        let base = serve(echo).await;
        let client = ReqwestConfig {
            user_agent: Some("hygiea-test/1.0".into()),
            ..local_config()
        }
        .build()
        .unwrap();
        let seen = echo_via(&client, format!("{base}/echo")).await;
        assert_eq!(seen["headers"]["user-agent"], "hygiea-test/1.0");
    }

    /// 默认头每个请求都带；单请求设了同名头时以单请求的为准
    #[tokio::test]
    async fn default_headers_are_sent_and_overridable() {
        let base = serve(echo).await;
        let client = local_config()
            .default_headers(header_map(&[("x-a", "1"), ("x-b", "1")]))
            .unwrap()
            .build()
            .unwrap();
        let seen = RequestConfig::plain(Method::GET, format!("{base}/echo"))
            .headers(header_map(&[("x-b", "2")]))
            .unwrap()
            .send::<Json<serde_json::Value>>(&client)
            .await
            .unwrap()
            .body;
        assert_eq!(seen["headers"]["x-a"], "1");
        assert_eq!(seen["headers"]["x-b"], "2");
    }

    /// 透明压缩打开时自动补 Accept-Encoding，关掉时不补
    #[tokio::test]
    async fn transparent_compression_toggles_accept_encoding() {
        let base = serve(echo).await;

        let on = local_config().build().unwrap();
        let seen = echo_via(&on, format!("{base}/echo")).await;
        let accept = seen["headers"]["accept-encoding"].as_str().unwrap();
        for algo in ["gzip", "br", "zstd", "deflate"] {
            assert!(accept.contains(algo), "{accept}");
        }

        let off = ReqwestConfig {
            transparent_compression: false,
            ..local_config()
        }
        .build()
        .unwrap();
        let seen = echo_via(&off, format!("{base}/echo")).await;
        assert!(seen["headers"].get("accept-encoding").is_none(), "{seen}");
    }
}

/// 重定向
mod redirects {
    use super::*;

    /// /redirect → 302 到 /echo，/loop → 302 到自己
    async fn redirect_server() -> String {
        serve(|req| match req.path.as_str() {
            "/redirect" => Reply::empty(302).header("location", "/echo"),
            "/loop" => Reply::empty(302).header("location", "/loop"),
            _ => echo(req),
        })
        .await
    }

    /// 默认跟随重定向：拿到的是 /echo 回的内容；`HttpResponse.url` 仍是最初发出的地址
    #[tokio::test]
    async fn followed_by_default() {
        let base = redirect_server().await;
        let resp = RequestConfig::plain(Method::GET, format!("{base}/redirect"))
            .send::<Json<serde_json::Value>>(&local_config().build().unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(resp.body["path"], "/echo");
        assert_eq!(resp.url.path(), "/redirect");
    }

    /// `max_redirects: 0` 时不跟随，3xx 原样回来，send 按非 2xx 报错，状态码在 err_args 里
    #[tokio::test]
    async fn disabled_returns_3xx() {
        let base = redirect_server().await;
        let client = ReqwestConfig {
            max_redirects: 0,
            ..local_config()
        }
        .build()
        .unwrap();
        // 不跟随时 302 原样回来，按非 2xx 报错
        let err = RequestConfig::plain(Method::GET, format!("{base}/redirect"))
            .send::<Bytes>(&client)
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::NonSuccessStatus), "{err:#}");
        assert_eq!(err.err_args()["status"], 302);
    }

    /// 超过上限报 BaseHttpErr::RequestFailed，原因是 reqwest 的重定向错误
    #[tokio::test]
    async fn exceeding_limit_is_error() {
        let base = redirect_server().await;
        let client = ReqwestConfig {
            max_redirects: 2,
            ..local_config()
        }
        .build()
        .unwrap();
        let err = RequestConfig::plain(Method::GET, format!("{base}/loop"))
            .send::<Bytes>(&client)
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::RequestFailed));
        let source = err.source().unwrap().downcast_ref::<reqwest::Error>();
        assert!(source.unwrap().is_redirect());
    }
}

/// cookie 罐子
mod cookies {
    use super::*;

    async fn cookie_server() -> String {
        serve(|req| match req.path.as_str() {
            "/login" => Reply::empty(200).header("set-cookie", "sid=abc; Path=/"),
            _ => echo(req),
        })
        .await
    }

    async fn cookie_seen_after_login(client: &Client, base: &str) -> serde_json::Value {
        RequestConfig::plain(Method::GET, format!("{base}/login"))
            .send::<Bytes>(client)
            .await
            .unwrap();
        echo_via(client, format!("{base}/echo")).await["headers"]["cookie"].clone()
    }

    /// 打开后，响应里的 Set-Cookie 会在后续同域请求上带回去
    #[tokio::test]
    async fn enabled_sends_cookie_back() {
        let base = cookie_server().await;
        let client = ReqwestConfig {
            cookie_store: true,
            ..local_config()
        }
        .build()
        .unwrap();
        assert_eq!(cookie_seen_after_login(&client, &base).await, "sid=abc");
    }

    /// 默认关闭，不带 cookie
    #[tokio::test]
    async fn disabled_by_default() {
        let base = cookie_server().await;
        let client = local_config().build().unwrap();
        assert!(cookie_seen_after_login(&client, &base).await.is_null());
    }
}

/// 写死的域名解析
mod resolve {
    use super::*;

    /// 域名按 resolve 解析到本地服务，端口用 URL 里的
    #[tokio::test]
    async fn overrides_dns() {
        let base = serve(echo).await;
        let addr: SocketAddr = base.trim_start_matches("http://").parse().unwrap();
        let client = local_config()
            .resolve("hygiea.test", vec![SocketAddr::new(addr.ip(), 1)])
            .build()
            .unwrap();
        let seen = echo_via(&client, format!("http://hygiea.test:{}/echo", addr.port())).await;
        assert_eq!(
            seen["headers"]["host"],
            format!("hygiea.test:{}", addr.port())
        );
    }
}

/// 超时：client 级基线和单请求覆盖
mod timeouts {
    use super::*;

    /// 延迟才回的服务；拉长到 2s 是为了给 50ms 的超时留够余量，CI 慢的时候也不会误判
    async fn slow_server() -> String {
        serve(|req| echo(req).delay(Duration::from_secs(2))).await
    }

    /// client 的整体超时生效，报 BaseHttpErr::RequestFailed，原因是 reqwest 的超时
    #[tokio::test]
    async fn client_timeout_applies() {
        let base = slow_server().await;
        let client = ReqwestConfig {
            timeout: Some(Duration::from_millis(50)),
            ..local_config()
        }
        .build()
        .unwrap();
        let err = RequestConfig::plain(Method::GET, format!("{base}/slow"))
            .send::<Bytes>(&client)
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::RequestFailed));
        assert!(is_timeout(&err));
    }

    /// 单请求可以把超时调大，不用单开 Client
    #[tokio::test]
    async fn request_timeout_can_extend() {
        let base = slow_server().await;
        let client = ReqwestConfig {
            timeout: Some(Duration::from_millis(50)),
            ..local_config()
        }
        .build()
        .unwrap();
        RequestConfig::plain(Method::GET, format!("{base}/slow"))
            .timeout(Duration::from_secs(5))
            .send::<Bytes>(&client)
            .await
            .unwrap();
    }

    /// 单请求也可以把超时调小
    #[tokio::test]
    async fn request_timeout_can_shrink() {
        let base = slow_server().await;
        let client = local_config().build().unwrap();
        let err = RequestConfig::plain(Method::GET, format!("{base}/slow"))
            .timeout(Duration::from_millis(50))
            .send::<Bytes>(&client)
            .await
            .unwrap_err();
        assert!(is_timeout(&err));
    }

    /// read_timeout：头很快到，body 隔了很久才来。整体 timeout / connect_timeout 都关掉，
    /// 确认真正生效的是 read_timeout，而不是别的超时旋钮
    #[tokio::test]
    async fn read_timeout_fires_when_body_is_delayed_after_headers() {
        let base = serve(|req| echo(req).body_delay(Duration::from_secs(2))).await;
        let client = ReqwestConfig {
            timeout: None,
            connect_timeout: None,
            read_timeout: Some(Duration::from_millis(100)),
            ..local_config()
        }
        .build()
        .unwrap();
        let err = RequestConfig::plain(Method::GET, format!("{base}/slow-body"))
            .send::<Bytes>(&client)
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::RequestFailed));
        assert!(is_timeout(&err));
    }

    /// connect_timeout：连一个只接受 TCP、从不回话的本地服务，用 https:// 让 TLS 握手卡住，建连阶段到点后报错。
    /// connect_timeout 覆盖 DNS、TCP 握手、TLS 握手三段；本机让 TCP 握手本身卡住做不到跨平台，所以卡在 TLS 这段，
    /// 见 `hygiea_test::tcp`
    #[tokio::test]
    async fn connect_timeout_fires_when_tls_handshake_stalls() {
        let target = hygiea_test::tcp::silent();
        let client = ReqwestConfig {
            connect_timeout: Some(Duration::from_millis(200)),
            ..local_config()
        }
        .build()
        .unwrap();
        let err = RequestConfig::plain(Method::GET, format!("{}/x", target.https_url()))
            .send::<Bytes>(&client)
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::RequestFailed));
        assert!(reqwest_source(&err).is_connect(), "{err:?}");
        assert!(is_timeout(&err));
    }
}

/// D3：reqwest 默认的重试行为。它只覆盖协议层 nack（本地 HTTP/1.1 测试服务碰不到），
/// 固定 5xx、连接被重置这些都不重试，用服务端计数把这一点钉住
mod retries {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// 固定返回 500：服务端只会收到一次请求
    #[tokio::test]
    async fn fixed_5xx_is_not_retried() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let base = serve(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Reply::json(500, "{}")
        })
        .await;
        let _ = RequestConfig::plain(Method::GET, format!("{base}/fail"))
            .send::<Bytes>(&local_config().build().unwrap())
            .await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// 连接被重置（RST）：同样不重试，只发一次请求
    #[tokio::test]
    async fn connection_reset_is_not_retried() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let base = serve(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Reply::reset()
        })
        .await;
        let _ = RequestConfig::plain(Method::GET, format!("{base}/reset"))
            .send::<Bytes>(&local_config().build().unwrap())
            .await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}

/// 显式代理：认证和代理头带给代理，排除列表和全局 no_proxy 让请求直连
mod proxies {
    use super::*;

    /// 代理服务：回 `{via: "proxy", headers}`，带 `via` 字段就说明请求走了代理
    async fn proxy_server() -> String {
        serve(|req| {
            let headers: serde_json::Map<_, _> = req
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone().into()))
                .collect();
            Reply::json(
                200,
                serde_json::json!({"via": "proxy", "headers": headers}).to_string(),
            )
        })
        .await
    }

    fn proxied(proxy: String) -> ReqwestConfig {
        let toml = format!("[[proxies]]\n{proxy}");
        toml::from_str(&toml).unwrap()
    }

    /// custom_http_auth 和 headers 都带给代理
    #[tokio::test]
    async fn auth_and_headers_are_sent_to_proxy() {
        let proxy = proxy_server().await;
        let client = proxied(format!(
            "kind = \"http\"\nurl = \"{proxy}\"\ncustom_http_auth = \"Bearer proxy-token\"\nheaders = {{ x-proxy-tag = \"app\" }}"
        ))
        .build()
        .unwrap();
        let seen = echo_via(&client, "http://example.invalid/path".into()).await;
        assert_eq!(seen["via"], "proxy");
        assert_eq!(seen["headers"]["proxy-authorization"], "Bearer proxy-token");
        assert_eq!(seen["headers"]["x-proxy-tag"], "app");
    }

    /// basic_auth 编码成 Basic 认证头
    #[tokio::test]
    async fn basic_auth_is_sent_to_proxy() {
        let proxy = proxy_server().await;
        let client = proxied(format!(
            "url = \"{proxy}\"\nbasic_auth = {{ username = \"user\", password = \"pass\" }}"
        ))
        .build()
        .unwrap();
        let seen = echo_via(&client, "http://example.invalid/path".into()).await;
        assert_eq!(seen["headers"]["proxy-authorization"], "Basic dXNlcjpwYXNz");
    }

    /// 代理自己的 no_proxy 命中时直连
    #[tokio::test]
    async fn proxy_exclusion_connects_directly() {
        let target = serve(echo).await;
        let proxy = proxy_server().await;
        let client = proxied(format!("url = \"{proxy}\"\nno_proxy = \"127.0.0.1\""))
            .build()
            .unwrap();
        let seen = echo_via(&client, format!("{target}/direct")).await;
        assert!(seen.get("via").is_none(), "{seen}");
    }

    /// kind = "https" 的代理不管 http 请求
    #[tokio::test]
    async fn https_proxy_ignores_http_requests() {
        let target = serve(echo).await;
        let proxy = proxy_server().await;
        let client = proxied(format!("kind = \"https\"\nurl = \"{proxy}\""))
            .build()
            .unwrap();
        let seen = echo_via(&client, format!("{target}/direct")).await;
        assert!(seen.get("via").is_none(), "{seen}");
    }

    /// 全局 no_proxy = true 时连显式配的代理也不用
    #[tokio::test]
    async fn global_no_proxy_overrides_proxies() {
        let target = serve(echo).await;
        let proxy = proxy_server().await;
        let client = ReqwestConfig {
            no_proxy: true,
            ..proxied(format!("url = \"{proxy}\""))
        }
        .build()
        .unwrap();
        let seen = echo_via(&client, format!("{target}/direct")).await;
        assert!(seen.get("via").is_none(), "{seen}");
    }
}
