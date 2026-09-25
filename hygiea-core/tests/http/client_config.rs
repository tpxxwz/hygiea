//! `ClientConfig` 的各个选项在真实请求上的效果

use std::error::Error as _;
use std::net::SocketAddr;
use std::time::Duration;

use hygiea_core::net::http::*;

use crate::support::*;

/// 发一个 GET 到回显服务，拿回服务端看到的请求
async fn echo_via(client: &Client, url: String) -> serde_json::Value {
    RequestConfig::plain(Method::GET, url)
        .send::<Json<serde_json::Value>>(client)
        .await
        .unwrap()
        .body
        .0
}

/// 请求头相关：User-Agent、默认头、透明压缩
mod headers {
    use super::*;

    /// user_agent 会带在每个请求上
    #[tokio::test]
    async fn user_agent_is_sent() {
        let base = serve(echo).await;
        let client = ClientConfig {
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
            .body
            .0;
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

        let off = ClientConfig {
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
        assert_eq!(resp.body.0["path"], "/echo");
        assert_eq!(resp.url.path(), "/redirect");
    }

    /// `max_redirects: None` 时不跟随，3xx 原样回来，send 按非 2xx 报错，状态码在 err_args 里
    #[tokio::test]
    async fn disabled_returns_3xx() {
        let base = redirect_server().await;
        let client = ClientConfig {
            max_redirects: None,
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
        let client = ClientConfig {
            max_redirects: Some(2),
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
        let client = ClientConfig {
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

    /// 延迟 300ms 才回的服务
    async fn slow_server() -> String {
        serve(|req| echo(req).delay(Duration::from_millis(300))).await
    }

    fn is_timeout(err: &hygiea_core::HyErr) -> bool {
        err.source()
            .and_then(|e| e.downcast_ref::<reqwest::Error>())
            .is_some_and(|e| e.is_timeout())
    }

    /// client 的整体超时生效，报 BaseHttpErr::RequestFailed，原因是 reqwest 的超时
    #[tokio::test]
    async fn client_timeout_applies() {
        let base = slow_server().await;
        let client = ClientConfig {
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
        let client = ClientConfig {
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
}
