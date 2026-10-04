//! 日志：失败日志级别 [`FailedLogLevel`]，以及 send 用的 start / success / failed 三种日志。
//! 字节怎么转成日志里的文本、在哪一步打码见 text.rs 开头。
//!
//! debug（`debug-log` feature 下 [`ReqwestConfig::debug`](super::ReqwestConfig) 打开）时不走这三种，
//! 改走 [`log_debug`]：消息后面接一段 JSON（默认一行，`set_pretty` 打开后缩进成多行）。JSON 的结构是 [`DebugStart`] / [`DebugEnd`]，
//! 和上面三种各自定义、互不影响

use hygiea_core::HyErr;
#[cfg(feature = "debug-log")]
use hygiea_core::{BaseErr, Result, ResultExt, err};

use super::request::HttpResponse;
use super::{Level, Method, StatusCode};

/// 请求失败时的日志级别。只开放 `Warn` 和 `Error` 两档——失败走 `Info` 及以下会淹没在正常日志里，
/// 所以不给这个选项，而不是靠约定。用 `Level::from(..)` 转成 tracing 的级别。
///
/// tracing 的级别是静态 callsite 元数据的一部分，不能把变量直接传给 `event!`，
/// 所以实现里仍然是 match 分派到 `warn!` / `error!` 两个宏
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailedLogLevel {
    #[default]
    Warn,
    Error,
}

impl From<FailedLogLevel> for Level {
    fn from(value: FailedLogLevel) -> Self {
        match value {
            FailedLogLevel::Warn => Level::WARN,
            FailedLogLevel::Error => Level::ERROR,
        }
    }
}

// ---------------- 拿到响应之后的日志（send 用）----------------
//
// `req` 是请求那一侧的字段，`resp_preview` 是响应摘要

/// 每条日志里请求那一侧的字段：打码后的地址和请求摘要（`req=` 那一段）
#[derive(Clone, Copy)]
pub(super) struct ReqFields<'a> {
    pub(super) url: &'a str,
    pub(super) req: &'a str,
}

/// 成功。调用前自己判断 enable_logging，省得关了日志还去算预览
pub(super) fn log_resp_success<Resp>(
    resp: &HttpResponse<Resp>,
    req: ReqFields<'_>,
    resp_preview: Option<&str>,
) {
    log_success(&Success {
        method: &resp.method,
        url: req.url,
        status: resp.status,
        elapsed_ms: resp.elapsed.as_millis(),
        req: req.req,
        resp: resp_preview,
    });
}

/// 非 2xx，不管 enable_logging 都打
pub(super) fn log_status_failed<Resp>(
    level: FailedLogLevel,
    resp: &HttpResponse<Resp>,
    req: ReqFields<'_>,
    resp_preview: &str,
) {
    let failure = Failure {
        resp: Some(resp_preview),
        ..failure_of(resp, req)
    };
    // 和传输失败的 "http call failed" 区分开，按消息就能筛出非 2xx
    log_failed(level, "http call non-2xx", &failure);
}

/// 2xx 但解码失败：调用方拿到的是 Err，按失败打，带上原文方便对照
pub(super) fn log_decode_failed<Resp>(
    level: FailedLogLevel,
    resp: &HttpResponse<Resp>,
    req: ReqFields<'_>,
    err: &HyErr,
    resp_preview: &str,
) {
    let failure = Failure {
        resp: Some(resp_preview),
        error: Some(err),
        ..failure_of(resp, req)
    };
    log_failed(level, "http call decode failed", &failure);
}

/// 拿到了响应时共有的那几个字段
fn failure_of<'a, Resp>(resp: &'a HttpResponse<Resp>, req: ReqFields<'a>) -> Failure<'a> {
    Failure {
        status: Some(resp.status),
        elapsed_ms: Some(resp.elapsed.as_millis()),
        ..Failure::new(&resp.method, req)
    }
}

// ---------------- 三种日志：start / success / failed ----------------
//
// 每种一个字段结构体加一个入口函数，字段同名同序（method、url、status、elapsed_ms、req、resp），
// 日志平台上可以按同一套字段检索。打日志只走这三个入口

/// 请求发出之前的那条日志（INFO）
pub(super) struct Start<'a> {
    pub(super) method: &'a Method,
    pub(super) req: ReqFields<'a>,
}

/// 打 `http call start`
pub(super) fn log_start(s: &Start<'_>) {
    tracing::info!(method = %s.method, url = %s.req.url, req = %s.req.req, "http call start");
}

/// 成功的那条日志（INFO）。`resp` 是 `None` 时不输出：类型没提供（打码后的）预览
struct Success<'a> {
    method: &'a Method,
    url: &'a str,
    status: StatusCode,
    /// 发出请求到 body 交给调用方的耗时
    elapsed_ms: u128,
    req: &'a str,
    resp: Option<&'a str>,
}

/// 打 `http call success`
fn log_success(s: &Success<'_>) {
    tracing::info!(
        method = %s.method,
        url = %s.url,
        status = %s.status,
        elapsed_ms = s.elapsed_ms,
        req = %s.req,
        resp = s.resp.map(tracing::field::display),
        "http call success"
    );
}

/// 一条失败日志的字段。和成功日志同名同序（method、url、status、elapsed_ms、req、resp），
/// 多一个 error；是 `None` 的字段不输出，比如传输失败时没有 status 和 resp，构建失败时没有耗时
pub(super) struct Failure<'a> {
    pub(super) method: &'a Method,
    pub(super) url: &'a str,
    pub(super) status: Option<StatusCode>,
    /// 请求发出后到失败为止的耗时
    pub(super) elapsed_ms: Option<u128>,
    pub(super) req: &'a str,
    pub(super) resp: Option<&'a str>,
    /// 按 `{:#}` 打，带上 source 链
    pub(super) error: Option<&'a HyErr>,
}

impl<'a> Failure<'a> {
    /// 只有每条失败日志都有的三项，其余为 `None`
    pub(super) fn new(method: &'a Method, req: ReqFields<'a>) -> Self {
        Self {
            method,
            url: req.url,
            status: None,
            elapsed_ms: None,
            req: req.req,
            resp: None,
            error: None,
        }
    }
}

/// 按配置的级别打一条结构化的失败日志。
///
/// tracing 的级别是静态 callsite 元数据的一部分，不能把变量传给 `event!`，只能 match 分派；
/// 用宏展开两个分支，保证 warn / error 两边的字段完全一致
pub(super) fn log_failed(level: FailedLogLevel, message: &str, f: &Failure<'_>) {
    let error = f.error.map(|e| format!("{e:#}"));
    macro_rules! emit {
        ($event:ident) => {
            tracing::$event!(
                method = %f.method,
                url = %f.url,
                status = f.status.map(tracing::field::display),
                elapsed_ms = f.elapsed_ms,
                req = %f.req,
                resp = f.resp.map(tracing::field::display),
                error = error.as_deref().map(tracing::field::display),
                "{message}"
            )
        };
    }
    match level {
        FailedLogLevel::Warn => emit!(warn),
        FailedLogLevel::Error => emit!(error),
    }
}

// ---------------- debug 日志 ----------------
//
// 每次请求两条：start（DebugStart）和结束（DebugEnd，请求加响应，单独一条就能看懂）。
// 序列化成 pretty JSON，接在消息后面。headers / params / body 的结构运行时才知道，是 `serde_json::Value`

/// 请求那一侧，start 和结束两条共用。不打码
#[cfg(feature = "debug-log")]
#[derive(serde::Serialize)]
pub(super) struct DebugRequest {
    /// 请求自己的头补上 client 默认头，同名多值是数组
    pub(super) headers: serde_json::Value,
    /// 没有是 null
    pub(super) params: serde_json::Value,
    /// JSON 的嵌套成对象，别的是字符串，没有是 null
    pub(super) body: serde_json::Value,
}

/// 响应
#[cfg(feature = "debug-log")]
#[derive(serde::Serialize)]
pub(super) struct DebugResponse {
    pub(super) headers: serde_json::Value,
    /// JSON 的嵌套成对象，别的是字符串；没读 body（`BodyStream`）是 null
    pub(super) body: serde_json::Value,
}

/// `http call start`
#[cfg(feature = "debug-log")]
#[derive(serde::Serialize)]
pub(super) struct DebugStart<'a> {
    pub(super) method: &'a str,
    /// 真正发出去的地址，params 不打码
    pub(super) url: &'a str,
    pub(super) request: &'a DebugRequest,
}

/// 结束那条：success / non-2xx / decode failed / failed 共用这一个形状
#[cfg(feature = "debug-log")]
#[derive(serde::Serialize)]
pub(super) struct DebugEnd<'a> {
    pub(super) method: &'a str,
    pub(super) url: &'a str,
    /// 传输失败（没拿到响应头）时是 null
    pub(super) status: Option<u16>,
    pub(super) elapsed_ms: u128,
    pub(super) request: &'a DebugRequest,
    /// 传输失败时是 null
    pub(super) response: Option<DebugResponse>,
    /// 按 `{:#}` 带 source 链；成功、非 2xx 时是 null
    pub(super) error: Option<String>,
}

/// debug 时的日志：消息后面接 JSON。`pretty` 时 JSON 另起一行、缩进成多行，否则整条一行。
/// `level` 为 `None` 时是 INFO（start / success），失败时按配置的级别。序列化失败返回 `JsonError`，不打日志
#[cfg(feature = "debug-log")]
pub(super) fn log_debug(
    level: Option<FailedLogLevel>,
    message: &str,
    pretty: bool,
    fields: &impl serde::Serialize,
) -> Result<()> {
    let json = if pretty {
        serde_json::to_string_pretty(fields)
    } else {
        serde_json::to_string(fields)
    }
    .wrap_err(|| err!(BaseErr::JsonError, "serialize debug log failed"))?;
    // pretty 时 JSON 另起一行；紧凑时整条一行（JSON 字符串里的换行已经转义成 \n）
    let sep = if pretty { "\n" } else { " " };
    match level {
        None => tracing::info!("{message}{sep}{json}"),
        Some(FailedLogLevel::Warn) => tracing::warn!("{message}{sep}{json}"),
        Some(FailedLogLevel::Error) => tracing::error!("{message}{sep}{json}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::reqwest_client::Url;
    use crate::reqwest_client::{Bytes, HeaderMap};
    use hygiea_core::{BaseErr, err};
    use std::time::Duration;
    use test_support::logs::capture;

    use super::*;

    /// `FailedLogLevel`
    mod failed_log_level {
        use super::*;

        /// 默认 Warn
        #[test]
        fn default_is_warn() {
            assert_eq!(FailedLogLevel::default(), FailedLogLevel::Warn);
        }

        /// 转成 tracing 的级别
        #[test]
        fn converts_to_tracing_level() {
            assert_eq!(Level::from(FailedLogLevel::Warn), Level::WARN);
            assert_eq!(Level::from(FailedLogLevel::Error), Level::ERROR);
        }
    }

    /// 实际打出来的日志：级别、消息和字段
    mod log_output {
        use super::*;

        fn resp() -> HttpResponse {
            HttpResponse {
                method: Method::POST,
                status: StatusCode::BAD_GATEWAY,
                headers: HeaderMap::new(),
                url: Url::parse("http://127.0.0.1/sent?q=1").unwrap(),
                elapsed: Duration::from_millis(42),
                body: Bytes::new(),
            }
        }

        /// 请求那一侧的字段
        fn req(url: &'static str, req: &'static str) -> ReqFields<'static> {
            ReqFields { url, req }
        }

        /// 和 resp() 对应的请求字段
        fn sent() -> ReqFields<'static> {
            req("http://127.0.0.1/sent?q=1", "[params:1]")
        }

        /// 捕获一次打日志的输出，只该有一行
        fn one_line(f: impl FnOnce()) -> String {
            let (out, _guard) = capture();
            f();
            let log = out.text();
            assert_eq!(log.lines().count(), 1, "{log}");
            log.trim_end().to_string()
        }

        /// 断言这些片段按给定顺序出现在同一行里
        fn assert_in_order(line: &str, parts: &[&str]) {
            let mut from = 0;
            for part in parts {
                match line[from..].find(part) {
                    Some(pos) => from += pos + part.len(),
                    None => panic!("missing or out of order: {part}\n{line}"),
                }
            }
        }

        /// `log_failed` 按级别分派到 warn / error，两边字段一样
        #[test]
        fn log_failed_uses_configured_level() {
            let method = Method::GET;
            let failure = Failure::new(&method, req("http://h/", "[]"));
            let warn = one_line(|| log_failed(FailedLogLevel::Warn, "http call failed", &failure));
            let error =
                one_line(|| log_failed(FailedLogLevel::Error, "http call failed", &failure));
            assert!(warn.contains(" WARN "), "{warn}");
            assert!(error.contains("ERROR "), "{error}");
            let fields = |line: &str| line[line.find("http call failed").unwrap()..].to_string();
            assert_eq!(fields(&warn), fields(&error));
        }

        /// 是 None 的字段不输出，只剩每条失败日志都有的 method / url / req
        #[test]
        fn none_fields_are_omitted() {
            let method = Method::GET;
            let line = one_line(|| {
                log_failed(
                    FailedLogLevel::Warn,
                    "http call failed",
                    &Failure::new(&method, req("http://h/", "[]")),
                )
            });
            assert_in_order(
                &line,
                &["http call failed", "method=GET", "url=http://h/", "req=[]"],
            );
            for absent in ["status=", "elapsed_ms=", "resp=", "error="] {
                assert!(!line.contains(absent), "{absent} should be absent: {line}");
            }
        }

        /// 字段都有时全部输出，error 按 `{:#}` 带上 source 链
        #[test]
        fn all_fields_are_structured() {
            let method = Method::PUT;
            let err = err!(BaseErr::JsonError, "outer").with_source(std::io::Error::other("inner"));
            let failure = Failure {
                status: Some(StatusCode::NOT_FOUND),
                elapsed_ms: Some(7),
                resp: Some("body"),
                error: Some(&err),
                ..Failure::new(&method, req("http://h/x", "[params:1]"))
            };
            let line = one_line(|| log_failed(FailedLogLevel::Warn, "http call failed", &failure));
            assert_in_order(
                &line,
                &[
                    "http call failed",
                    "method=PUT",
                    "url=http://h/x",
                    "status=404 Not Found",
                    "elapsed_ms=7",
                    "req=[params:1]",
                    "resp=body",
                    "error=JSON error: outer: inner",
                ],
            );
        }

        /// start 日志是 INFO，带方法、打码后的地址和请求预览，没有状态、耗时、响应
        #[test]
        fn start_log_fields() {
            let method = Method::GET;
            let line = one_line(|| {
                log_start(&Start {
                    method: &method,
                    req: req("http://h/x?token=***", "[params:1]"),
                })
            });
            assert!(line.contains(" INFO "), "{line}");
            assert_in_order(
                &line,
                &[
                    "http call start",
                    "method=GET",
                    "url=http://h/x?token=***",
                    "req=[params:1]",
                ],
            );
            for absent in ["status=", "elapsed_ms=", "resp="] {
                assert!(!line.contains(absent), "{absent} should be absent: {line}");
            }
        }

        /// 成功日志是 INFO，字段和失败日志同名同序
        #[test]
        fn success_log_fields() {
            let line = one_line(|| log_resp_success(&resp(), sent(), Some("resp-body")));
            assert!(line.contains(" INFO "), "{line}");
            assert_in_order(
                &line,
                &[
                    "http call success",
                    "method=POST",
                    "url=http://127.0.0.1/sent?q=1",
                    "status=502 Bad Gateway",
                    "elapsed_ms=42",
                    "req=[params:1]",
                    "resp=resp-body",
                ],
            );
        }

        /// 非 2xx：按配置的失败级别打，带状态、耗时、请求和响应预览，没有 error
        #[test]
        fn status_failed_log() {
            let line =
                one_line(|| log_status_failed(FailedLogLevel::Error, &resp(), sent(), "resp-body"));
            assert!(line.contains("ERROR "), "{line}");
            assert_in_order(
                &line,
                &[
                    "http call non-2xx",
                    "method=POST",
                    "url=http://127.0.0.1/sent?q=1",
                    "status=502 Bad Gateway",
                    "elapsed_ms=42",
                    "req=[params:1]",
                    "resp=resp-body",
                ],
            );
            assert!(!line.contains("error="), "{line}");
        }

        /// 解码失败：消息不同，多一个 error 字段
        #[test]
        fn decode_failed_log() {
            let err = err!(BaseErr::JsonError, "decode boom");
            let line = one_line(|| {
                log_decode_failed(FailedLogLevel::Warn, &resp(), sent(), &err, "resp-body")
            });
            assert!(line.contains(" WARN "), "{line}");
            assert_in_order(
                &line,
                &[
                    "http call decode failed",
                    "status=502 Bad Gateway",
                    "resp=resp-body",
                    "error=JSON error: decode boom",
                ],
            );
        }
    }

    /// `log_debug`：消息后面换行接 pretty JSON，字段按结构体的顺序，级别按参数
    #[cfg(feature = "debug-log")]
    mod debug {
        use serde_json::json;

        use super::*;

        fn request() -> DebugRequest {
            DebugRequest {
                headers: json!({"accept": "*/*"}),
                params: serde_json::Value::Null,
                body: json!({"a": 1}),
            }
        }

        #[test]
        fn start_is_pretty_json() {
            let request = request();
            let (out, _guard) = capture();
            log_debug(
                None,
                "http call start",
                true,
                &DebugStart {
                    method: "GET",
                    url: "http://h/x",
                    request: &request,
                },
            )
            .unwrap();
            let log = out.text();
            assert!(log.contains(" INFO "), "{log}");
            let expected = r#"http call start
{
  "method": "GET",
  "url": "http://h/x",
  "request": {
    "headers": {
      "accept": "*/*"
    },
    "params": null,
    "body": {
      "a": 1
    }
  }
}"#;
            assert!(log.contains(expected), "{log}");
        }

        /// 结束那条：失败按级别打，没拿到响应时 status / response 是 null
        #[test]
        fn end_without_response() {
            let request = request();
            let (out, _guard) = capture();
            log_debug(
                Some(FailedLogLevel::Error),
                "http call failed",
                true,
                &DebugEnd {
                    method: "GET",
                    url: "http://h/x",
                    status: None,
                    elapsed_ms: 3,
                    request: &request,
                    response: None,
                    error: Some("boom".into()),
                },
            )
            .unwrap();
            let log = out.text();
            assert!(log.contains("ERROR "), "{log}");
            assert!(
                log.contains("\"status\": null,\n  \"elapsed_ms\": 3,"),
                "{log}"
            );
            assert!(
                log.contains("\"response\": null,\n  \"error\": \"boom\"\n}"),
                "{log}"
            );
        }

        /// 默认（不 pretty）：消息和 JSON 在同一行，空格隔开
        #[test]
        fn compact_is_one_line() {
            let request = request();
            let (out, _guard) = capture();
            log_debug(
                None,
                "http call start",
                false,
                &DebugStart {
                    method: "GET",
                    url: "http://h/x",
                    request: &request,
                },
            )
            .unwrap();
            let log = out.text();
            assert_eq!(log.lines().count(), 1, "{log}");
            let expected = r#"http call start {"method":"GET","url":"http://h/x","request":{"headers":{"accept":"*/*"},"params":null,"body":{"a":1}}}"#;
            assert!(log.contains(expected), "{log}");
        }

        /// 序列化失败：返回 JsonError，不打日志
        #[test]
        fn serialize_failure_is_err() {
            let (out, _guard) = capture();
            let err = log_debug(
                None,
                "http call start",
                false,
                &test_support::Unserializable,
            )
            .unwrap_err();
            assert!(err.is(hygiea_core::BaseErr::JsonError), "{err:#}");
            assert_eq!(out.text(), "");
        }
    }
}
