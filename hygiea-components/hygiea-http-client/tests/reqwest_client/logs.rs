//! 日志：什么时候打、打在什么级别、开关怎么影响

use hygiea_http_client::reqwest_client::*;

use crate::support::*;

const ROUTES: &[(&str, u16, &str)] = &[("/ok", 200, r#"{"ok":true}"#), ("/fail", 503, "down")];

/// 正常请求的 start / success 两条日志
mod normal {
    use super::*;

    /// 默认打 start 和 success 两条 INFO，带方法、地址、状态和请求/响应预览
    #[tokio::test]
    async fn start_and_success_are_logged() {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        RequestConfig::with_params(Method::GET, format!("{base}/ok"), [("q", "1")])
            .send::<Bytes>(&local_config().build().unwrap())
            .await
            .unwrap();
        let log = out.text();
        let lines: Vec<_> = log.lines().collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert!(
            lines[0].contains("INFO") && lines[0].contains("http call start"),
            "{log}"
        );
        assert!(
            lines[1].contains("INFO") && lines[1].contains("http call success"),
            "{log}"
        );
        assert!(lines[1].contains("method=GET"), "{log}");
        assert!(lines[1].contains("status=200 OK"), "{log}");
        assert!(lines[1].contains(r#"req=[params:[["q","1"]]]"#), "{log}");
        // Bytes 没有类型信息、没法打码，成功时不输出 resp
        assert!(!lines[1].contains("resp="), "{log}");
        assert!(lines[1].contains("elapsed_ms="), "{log}");
    }

    /// 用 String 接收时成功日志打文本原文（Bytes 不打，见上面）
    #[tokio::test]
    async fn string_response_is_logged() {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        RequestConfig::plain(Method::GET, format!("{base}/ok"))
            .send::<String>(&local_config().build().unwrap())
            .await
            .unwrap();
        let log = out.text();
        assert!(log.contains(r#"resp={"ok":true}"#), "{log}");
    }

    /// 关掉 enable_logging 后正常日志一条都不打
    #[tokio::test]
    async fn disabled_logging_is_silent_on_success() {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        RequestConfig::plain(Method::GET, format!("{base}/ok"))
            .enable_logging(false)
            .send::<Bytes>(&local_config().build().unwrap())
            .await
            .unwrap();
        assert_eq!(out.text(), "");
    }

    /// 关掉 enable_logging 后，流式响应（BodyStream）同样不打正常日志，跟一次性读完 body 的类型一致
    #[tokio::test]
    async fn disabled_logging_is_silent_for_streamed_response() {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        let mut r = RequestConfig::plain(Method::GET, format!("{base}/ok"))
            .enable_logging(false)
            .send::<BodyStream>(&local_config().build().unwrap())
            .await
            .unwrap();
        while let Some(chunk) = r.body.next().await {
            chunk.unwrap();
        }
        assert_eq!(out.text(), "");
    }
}

/// 失败日志：不受 enable_logging 影响，级别由 failed_log_level 决定。
/// 非 2xx 默认是 WARN、可配成 ERROR 这两点分别由 `logging.rs` 的
/// `log_failed_uses_configured_level` 单元测试和 `request.rs` 里 `failed_log_level`
/// 默认值/setter 的单元测试覆盖了，这里不重复，只留一条端到端确认失败日志确实会打
mod failures {
    use super::*;

    /// 关掉 enable_logging 也照样打失败日志，只是没有 start 那条
    #[tokio::test]
    async fn failures_are_logged_even_when_disabled() {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        let _ = RequestConfig::plain(Method::GET, format!("{base}/fail"))
            .enable_logging(false)
            .send::<Bytes>(&local_config().build().unwrap())
            .await;
        let log = out.text();
        assert_eq!(log.lines().count(), 1, "{log}");
        assert!(log.contains("http call non-2xx"), "{log}");
    }

    /// 传输层失败（连不上）也打失败日志，带耗时和请求预览
    #[tokio::test]
    async fn transport_failure_is_logged() {
        let base = closed_port_url().await;
        let (out, _guard) = capture();
        let _ = RequestConfig::with_params(Method::GET, format!("{base}/x"), [("q", "1")])
            .send::<Bytes>(&local_config().build().unwrap())
            .await;
        let log = out.text();
        assert!(log.contains("WARN"), "{log}");
        assert!(log.contains(r#"req=[params:[["q","1"]]]"#), "{log}");
        assert!(log.contains("elapsed_ms="), "{log}");
    }
}

/// 打日志本身失败（成功日志的响应预览序列化不了）：错误传给调用方，不吞掉
mod log_errors {
    use hygiea_core::BaseErr;
    use serde::{Deserialize, Serialize};
    use test_support::Unserializable;

    use super::*;

    /// 能解码、但重新序列化（成功日志的预览）必定失败的类型
    #[derive(Debug, Deserialize)]
    struct DecodeOnly {}

    impl Serialize for DecodeOnly {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            Unserializable.serialize(s)
        }
    }

    /// 请求成功了，但成功日志的预览序列化失败：send 返回 JsonError，带重试时也一样（不重试）
    #[tokio::test]
    async fn preview_failure_is_returned() {
        let base = serve_routes(ROUTES).await;
        let client = local_config().build().unwrap();
        let err = RequestConfig::plain(Method::GET, format!("{base}/ok"))
            .send::<Json<DecodeOnly>>(&client)
            .await
            .unwrap_err();
        assert!(err.is(BaseErr::JsonError), "{err:#}");
        let retry = |_: usize, _: RequestConfig, f: SendFailure| async move {
            RetryDecision::<(), ()>::Stop(f.err)
        };
        let err = RequestConfig::plain(Method::GET, format!("{base}/ok"))
            .send::<Json<DecodeOnly>>((&client, RetryCtx::new(3, retry)))
            .await
            .unwrap_err();
        assert!(err.is(BaseErr::JsonError), "{err:#}");
    }

    /// 关了日志就不算预览，也就不会因为它失败
    #[tokio::test]
    async fn no_preview_when_logging_disabled() {
        let base = serve_routes(ROUTES).await;
        RequestConfig::plain(Method::GET, format!("{base}/ok"))
            .enable_logging(false)
            .send::<Json<DecodeOnly>>(&local_config().build().unwrap())
            .await
            .unwrap();
    }
}
