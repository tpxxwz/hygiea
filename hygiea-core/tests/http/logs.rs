//! 日志：什么时候打、打在什么级别、开关怎么影响

use hygiea_core::net::http::*;

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
}

/// 失败日志：不受 enable_logging 影响，级别由 failed_log_level 决定
mod failures {
    use super::*;

    /// 非 2xx 默认打一条 WARN，状态和响应原文都是单独的结构化字段
    #[tokio::test]
    async fn non_2xx_is_warn_by_default() {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        let _ = RequestConfig::plain(Method::GET, format!("{base}/fail"))
            .send::<Bytes>(&local_config().build().unwrap())
            .await;
        let log = out.text();
        assert!(log.contains("WARN"), "{log}");
        assert!(log.contains("http call non-2xx"), "{log}");
        assert!(log.contains("status=503 Service Unavailable"), "{log}");
        assert!(log.contains("resp=down"), "{log}");
        assert!(!log.contains("http call success"), "{log}");
    }

    /// failed_log_level 设成 Error 后用 ERROR 级别
    #[tokio::test]
    async fn failed_log_level_error() {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        let _ = RequestConfig::plain(Method::GET, format!("{base}/fail"))
            .failed_log_level(FailedLogLevel::Error)
            .send::<Bytes>(&local_config().build().unwrap())
            .await;
        let log = out.text();
        assert!(log.contains("ERROR"), "{log}");
        assert!(!log.contains("WARN"), "{log}");
    }

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
