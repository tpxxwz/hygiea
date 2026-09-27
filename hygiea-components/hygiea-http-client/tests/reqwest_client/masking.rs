//! 日志里的打码：请求和响应里标了 `#[redact(mask)]` 的字段不出现原文，发出去和收回来的内容不受影响

use hygiea_http_client::reqwest_client::*;
use serde::{Deserialize, Serialize};

use crate::support::*;

/// 请求：params 和 body
mod request {
    use super::*;

    /// Json body 在 start 和 success 日志里都打码，服务端收到的是原文
    #[tokio::test]
    async fn json_body_is_masked_in_logs() {
        let base = serve(echo).await;
        let (out, _guard) = capture();
        let seen = RequestConfig::with_body(
            Method::POST,
            format!("{base}/login"),
            Json(login("secret-1")),
        )
        .send::<Json<serde_json::Value>>(&local_config().build().unwrap())
        .await
        .unwrap();
        // 服务端收到原文
        assert!(seen.body.0["body"].as_str().unwrap().contains("secret-1"));
        // 日志里 req 段打码；回显的响应是 Value，没有打码规则，所以只看 req 段
        let log = out.text();
        let start = log.lines().find(|l| l.contains("http call start")).unwrap();
        assert!(start.contains(r#""token":"***""#), "{start}");
        assert!(!start.contains("secret-1"), "{start}");
    }

    /// params 在日志的 req 段里打码
    #[tokio::test]
    async fn params_are_masked_in_req_segment() {
        let base = serve(echo).await;
        let (out, _guard) = capture();
        RequestConfig::with_params(Method::GET, format!("{base}/q"), login("secret-2"))
            .send::<Bytes>(&local_config().build().unwrap())
            .await
            .unwrap();
        let log = out.text();
        assert!(
            log.contains(r#"req=[params:{"user":"alice","token":"***"}]"#),
            "{log}"
        );
    }
}

/// 地址里的 query：params 按同样的规则打码，日志、HttpResponse.url 和错误里都一样
mod url {
    use super::*;

    const MASKED_QUERY: &str = "user=alice&token=***";

    /// 日志里的 url 字段 query 打码，服务端收到的是原文
    #[tokio::test]
    async fn logs_and_response_url_are_masked() {
        let base = serve(echo).await;
        let (out, _guard) = capture();
        let resp = RequestConfig::with_params(Method::GET, format!("{base}/q"), login("secret-q"))
            .send::<Json<serde_json::Value>>(&local_config().build().unwrap())
            .await
            .unwrap();
        // 服务端收到原文
        assert_eq!(resp.body.0["query"], "user=alice&token=secret-q");
        // HttpResponse.url 打码
        assert_eq!(resp.url.query(), Some(MASKED_QUERY));
        // 日志里只看 start 那条（success 那条会带上回显的响应原文）
        let log = out.text();
        let start = log.lines().find(|l| l.contains("http call start")).unwrap();
        assert!(
            start.contains(&format!("url={base}/q?{MASKED_QUERY}")),
            "{start}"
        );
        assert!(!start.contains("secret-q"), "{start}");
    }

    /// 非 2xx：失败日志和 send 的错误里都是打码后的地址
    #[tokio::test]
    async fn status_errors_carry_masked_url() {
        let base = serve(|_| Reply::json(500, "{}")).await;
        let client = local_config().build().unwrap();
        let url = format!("{base}/fail");
        let expected = format!("{url}?{MASKED_QUERY}");

        let (out, _guard) = capture();
        let err = RequestConfig::with_params(Method::GET, &url, login("secret-e"))
            .send::<Json<Login>>(&client)
            .await
            .unwrap_err();
        assert_eq!(err.err_args()["url"], expected);
        let log = out.text();
        assert!(
            log.contains(&expected) && !log.contains("secret-e"),
            "{log}"
        );
    }

    /// 传输层失败（连不上）：错误和失败日志里同样是打码后的地址
    #[tokio::test]
    async fn transport_errors_carry_masked_url() {
        let base = closed_port_url().await;
        let (out, _guard) = capture();
        let err = RequestConfig::with_params(Method::GET, format!("{base}/x"), login("secret-t"))
            .send::<Bytes>(&local_config().build().unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.err_args()["url"], format!("{base}/x?{MASKED_QUERY}"));
        let log = out.text();
        assert!(
            log.contains("token=***") && !log.contains("secret-t"),
            "{log}"
        );
    }

    /// 请求带 params 又经过一次重定向：日志和 HttpResponse.url 里的 query 仍然打码，
    /// 不会因为多走了一跳就漏出原文
    #[tokio::test]
    async fn masked_query_survives_redirect() {
        let base = serve(|req| match req.path.as_str() {
            // 把原始 query（未打码，服务端看到的是原文）原样带到重定向目标上，
            // 这样 /echo 收到的请求里还能看到 query，用来确认服务端收到的是原文
            "/start" => {
                let location = match &req.query {
                    Some(q) => format!("/echo?{q}"),
                    None => "/echo".to_string(),
                };
                Reply::empty(302).header("location", &location)
            }
            _ => echo(req),
        })
        .await;
        let (out, _guard) = capture();
        let resp =
            RequestConfig::with_params(Method::GET, format!("{base}/start"), login("secret-r"))
                .send::<Json<serde_json::Value>>(&local_config().build().unwrap())
                .await
                .unwrap();
        // 重定向目标（服务端）收到的是原文
        assert_eq!(resp.body.0["query"], "user=alice&token=secret-r");
        // HttpResponse.url 是最初发出的地址，query 仍是打码后的
        assert_eq!(resp.url.query(), Some(MASKED_QUERY));
        let log = out.text();
        let start = log.lines().find(|l| l.contains("http call start")).unwrap();
        assert!(
            start.contains(MASKED_QUERY) && !start.contains("secret-r"),
            "{start}"
        );
    }
}

/// Authorization：bearer / basic 凭据只走专门的 `auth`，不进请求日志的 req= 段（那里只有
/// params/body），非 2xx 时也不会跟着错误信息或失败日志一起漏出来
mod auth {
    use super::*;

    /// 非 2xx 触发失败日志和错误时，bearer / basic 的凭据都不出现在日志或错误里
    #[tokio::test]
    async fn credentials_do_not_leak_on_failure() {
        let base = serve(|_| Reply::json(500, r#"{"error":"denied"}"#)).await;
        let client = local_config().build().unwrap();
        for auth in [
            Auth::Bearer("s3cr3t-token".into()),
            Auth::Basic {
                username: "alice".into(),
                password: Some("s3cr3t-pass".into()),
            },
        ] {
            let (out, _guard) = capture();
            let err = RequestConfig::plain(Method::GET, format!("{base}/x"))
                .auth(auth)
                .send::<Bytes>(&client)
                .await
                .unwrap_err();
            assert!(err.is(BaseHttpErr::NonSuccessStatus));
            let rendered = format!("{err:#}");
            let log = out.text();
            assert!(!rendered.contains("s3cr3t"), "{rendered}");
            assert!(!log.contains("s3cr3t"), "{log}");
        }
    }
}

/// 响应：解码成功后按类型打码，其余情况打原文
mod response {
    use super::*;

    const ROUTES: &[(&str, u16, &str)] = &[
        ("/one", 200, r#"{"user":"alice","token":"secret-a"}"#),
        (
            "/list",
            200,
            r#"[{"user":"alice","token":"secret-a"},{"user":"bob","token":"secret-b"}]"#,
        ),
        (
            "/wrapped",
            200,
            r#"{"code":"0","data":[{"user":"alice","token":"secret-a"}]}"#,
        ),
    ];

    /// 泛型外层，和交易所常见的 {code, data} 一样，外层自己不用标
    #[derive(Debug, Serialize, Deserialize)]
    struct Wrapped<T> {
        code: String,
        data: T,
    }

    async fn send<Resp: FromBody + Send>(
        path: &str,
    ) -> (Result<HttpResponse<Resp>, hygiea_core::HyErr>, String) {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        let result = RequestConfig::plain(Method::GET, format!("{base}{path}"))
            .send::<Resp>(&local_config().build().unwrap())
            .await;
        (result, out.text())
    }

    /// 单个对象：调用方拿到原文，日志里打码
    #[tokio::test]
    async fn single_object() {
        let (r, log) = send::<Json<Login>>("/one").await;
        assert_eq!(r.unwrap().body.0.token, "secret-a");
        assert!(log.contains(r#""token":"***""#), "{log}");
        assert!(!log.contains("secret-a"), "{log}");
    }

    /// 顶层数组：每个元素都打码
    #[tokio::test]
    async fn top_level_array() {
        let (r, log) = send::<Json<Vec<Login>>>("/list").await;
        assert_eq!(r.unwrap().body.0[1].token, "secret-b");
        assert_eq!(log.matches(r#""token":"***""#).count(), 2, "{log}");
        assert!(!log.contains("secret-"), "{log}");
    }

    /// 泛型外层里的数组元素也打码
    #[tokio::test]
    async fn inside_generic_envelope() {
        let (r, log) = send::<Json<Wrapped<Vec<Login>>>>("/wrapped").await;
        assert_eq!(r.unwrap().body.0.data[0].token, "secret-a");
        assert!(log.contains(r#""token":"***""#), "{log}");
        assert!(!log.contains("secret-a"), "{log}");
    }

    /// 非 2xx 响应也按 charset 解码：GBK 编码的错误信息原样（解码后）进入 err_args["body"]，
    /// 不是打码后的占位符，也不是乱码
    #[tokio::test]
    async fn non_2xx_gbk_body_is_decoded_in_error() {
        let gbk = [0xc4, 0xe3, 0xba, 0xc3]; // "你好" 的 GBK 字节
        let base = serve(move |_| Reply::bytes(500, "text/plain; charset=gbk", gbk.to_vec())).await;
        let err = RequestConfig::plain(Method::GET, format!("{base}/gbk-fail"))
            .send::<Bytes>(&local_config().build().unwrap())
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::NonSuccessStatus));
        assert_eq!(err.err_args()["body"], "你好");
    }
}
