//! debug 日志（`debug-log` feature）。整个模块只在这个 feature 下编译（mod.rs 里的
//! `#[cfg(feature = "debug-log")] mod debug;`），模块里不再写 cfg；别的文件只在字段和调用处有 cfg。
//!
//! | 内容 | 说明 |
//! |---|---|
//! | [`ReqwestConfig::debug`]、[`ReqwestClient::debug`]、[`ReqwestClient::set_debug`]、[`ReqwestClient::set_pretty`] | 对外的开关 |
//! | `DebugState` | client 级的状态（默认头、两个开关），所有 clone 共享 |
//! | `DebugDraft` → `DebugCtx` | 一次请求：发出前取 params / body 原文，build 出请求后补上 url 和请求头 |
//! | `DebugStart` / `DebugEnd` | 两条日志的 JSON 结构：start 是请求，结束是请求加响应 |
//! | `headers_json` / `text_json` | 头转成 JSON 对象；文本能解析成 JSON 就嵌套，否则是字符串 |
//!
//! 打开后 send 不打平时的日志，改打这两条：消息后面接 JSON（默认一行，`set_pretty` 打开后缩进成多行），
//! 无视 `enable_logging`，不打码。序列化失败返回 `JsonError`，一路传给 send

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use reqwest::header::{ACCEPT, Entry, HeaderValue, USER_AGENT};
use serde::Serialize;
use serde_json::Value;

use hygiea_core::app::AppErr;
use hygiea_core::{BaseErr, HyErr, Result, ResultExt, err};

use super::body::IntoBody;
use super::config::HeaderMapConfig;
use super::logging::FailedLogLevel;
use super::text::body_text;
use super::{Bytes, HeaderMap, ReqwestClient, ReqwestConfig, StatusCode};

// ---------------------------- 对外的开关 ----------------------------

impl ReqwestConfig {
    /// 打开或关闭 debug 日志，见 [`ReqwestConfig::debug`](#structfield.debug)。只在 `debug-log` feature 下存在
    pub fn debug(mut self, on: bool) -> Self {
        self.debug = on;
        self
    }
}

impl ReqwestClient {
    /// 默认配置、打开 debug 日志的 client，就是 `ReqwestConfig::default().debug(true).build()`。
    /// 只在 `debug-log` feature 下存在，给测试用；要改别的配置就用 [`ReqwestConfig::debug`](ReqwestConfig::debug) 那条写法
    pub fn debug() -> Result<Self> {
        ReqwestConfig::default().debug(true).build()
    }

    /// 打开或关闭 debug 日志，初始值是 [`ReqwestConfig::debug`](ReqwestConfig::debug)。
    /// 原地改，这个 client 的所有 clone（包括组件放进 Resources 的那份）一起变；已经发出的请求不受影响。
    /// 只在 `debug-log` feature 下存在
    pub fn set_debug(&self, on: bool) {
        self.debug.debug.store(on, Ordering::Relaxed);
    }

    /// debug 日志的 JSON 要不要缩进成多行，默认关：一行紧凑的 JSON，消息和 JSON 之间是空格。
    /// 和 [`ReqwestClient::set_debug`] 一样原地改、所有 clone 一起变。只在 `debug-log` feature 下存在
    pub fn set_pretty(&self, on: bool) {
        self.debug.pretty.store(on, Ordering::Relaxed);
    }
}

// ---------------------------- client 级的状态 ----------------------------

/// client 级的 debug 状态，放在 `ReqwestClient` 的 `debug` 字段里，所有 clone 共享
#[derive(Debug)]
pub(super) struct DebugState {
    /// 建立时交给 reqwest 的 client 级默认头，之后不会变。日志里用来补全请求头
    default_headers: HeaderMap,
    /// 初始值来自 `ReqwestConfig` 的 `debug` 字段
    debug: AtomicBool,
    /// JSON 缩进成多行，默认关
    pretty: AtomicBool,
}

impl DebugState {
    /// build 时调，`user_agent` / `default_headers` 和交给 reqwest 的是同一份配置
    pub(super) fn new(
        debug: bool,
        user_agent: Option<String>,
        default_headers: HeaderMapConfig,
    ) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            default_headers: client_default_headers(user_agent, default_headers)?,
            debug: AtomicBool::new(debug),
            pretty: AtomicBool::new(false),
        }))
    }

    /// 这次请求的设置：`None` 表示 debug 关，`Some(pretty)` 表示开。每次请求在发出前读一次
    pub(super) fn mode(&self) -> Option<bool> {
        self.debug
            .load(Ordering::Relaxed)
            .then(|| self.pretty.load(Ordering::Relaxed))
    }
}

/// 照 reqwest（0.13）`ClientBuilder` 的规则算出 client 级默认头：
/// `ClientBuilder::new` 先放 `Accept: */*`，`user_agent` 再 insert `User-Agent`，
/// `default_headers` 最后逐个 insert——同名的覆盖前面的，同名多值只剩最后一个。
/// reqwest 没有读回默认头的接口，只能照抄；它的规则变了由集成测试里的对照（日志里的请求头 vs 服务端收到的）发现
fn client_default_headers(
    user_agent: Option<String>,
    default_headers: HeaderMapConfig,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    if let Some(ua) = user_agent {
        let ua = HeaderValue::try_from(ua)
            .map_err(|e| err!(AppErr::InvalidConfig, format!("user agent: {e}")))?;
        headers.insert(USER_AGENT, ua);
    }
    let configured = HeaderMap::try_from(default_headers)
        .map_err(|e| err!(AppErr::InvalidConfig, format!("default headers: {e}")))?;
    for (name, value) in &configured {
        headers.insert(name.clone(), value.clone());
    }
    Ok(headers)
}

// ---------------------------- 一次请求 ----------------------------

/// 发出前先取的 params / body 原文：`into_request` 会消费请求配置，要在那之前算
pub(super) struct DebugDraft {
    pretty: bool,
    params: Value,
    body: Value,
}

impl DebugDraft {
    /// params 和 body 不打码，没有的是 null；JSON 的 body 嵌套成对象。序列化失败返回 `JsonError`，请求不发出
    pub(super) fn new(pretty: bool, params: &impl Serialize, body: &impl IntoBody) -> Result<Self> {
        let params = serde_json::to_value(params)
            .wrap_err(|| err!(BaseErr::JsonError, "serialize params for debug log failed"))?;
        let body = match body.debug_preview()? {
            None => Value::Null,
            Some(body) => text_json(&body),
        };
        Ok(Self {
            pretty,
            params,
            body,
        })
    }

    /// build 出最终的请求之后：补上真正发出去的地址和请求头（请求自己的头补上 client 默认头，
    /// 只补请求里没有的名字，和 reqwest 发送时一样）
    pub(super) fn finish(self, request: &reqwest::Request, state: &DebugState) -> DebugCtx {
        let mut headers = request.headers().clone();
        for (name, value) in &state.default_headers {
            if let Entry::Vacant(entry) = headers.entry(name) {
                entry.insert(value.clone());
            }
        }
        DebugCtx {
            pretty: self.pretty,
            method: request.method().to_string(),
            url: request.url().to_string(),
            request: DebugRequest {
                headers: headers_json(&headers),
                params: self.params,
                body: self.body,
            },
            resp_body: OnceLock::new(),
        }
    }
}

/// 一次请求的 debug 日志要用的原文，放在 send 的 `SendCtx` 里
pub(super) struct DebugCtx {
    pretty: bool,
    method: String,
    /// 真正发出去的地址，params 不打码
    url: String,
    /// start 和结束两条共用
    request: DebugRequest,
    /// RespBody 读完 body 后存一份，成功日志打原文用
    resp_body: OnceLock<Bytes>,
}

impl DebugCtx {
    /// `http call start`：请求那一侧
    pub(super) fn log_start(&self) -> Result<()> {
        let start = DebugStart {
            method: &self.method,
            url: &self.url,
            request: &self.request,
        };
        log_debug(None, "http call start", self.pretty, &start)
    }

    /// 结束那条：请求加响应。`level` 为 `None` 是成功（INFO）。
    /// `response` 为 `None` 表示没拿到响应；body 不给时用 `keep_resp_body` 存下的那份，都没有就是 null
    pub(super) fn log_end(
        &self,
        level: Option<FailedLogLevel>,
        message: &str,
        status: Option<StatusCode>,
        response: Option<(&HeaderMap, Option<&Bytes>)>,
        error: Option<&HyErr>,
        elapsed_ms: u128,
    ) -> Result<()> {
        let response = response.map(|(headers, body)| DebugResponse {
            headers: headers_json(headers),
            body: body
                .or_else(|| self.resp_body.get())
                .map_or(Value::Null, |b| text_json(&body_text(headers, b))),
        });
        let end = DebugEnd {
            method: &self.method,
            url: &self.url,
            status: status.map(|s| s.as_u16()),
            elapsed_ms,
            request: &self.request,
            response,
            error: error.map(|e| format!("{e:#}")),
        };
        log_debug(level, message, self.pretty, &end)
    }

    /// RespBody 读完 body 后调，存一份给成功日志。只会存一次，再调不覆盖
    pub(super) fn keep_resp_body(&self, body: &Bytes) {
        let _ = self.resp_body.set(body.clone());
    }
}

// ---------------------------- 两条日志 ----------------------------
//
// headers / params / body 的结构运行时才知道，是 `serde_json::Value`

/// 请求那一侧，start 和结束两条共用。不打码
#[derive(Serialize)]
struct DebugRequest {
    /// 请求自己的头补上 client 默认头，同名多值是数组
    headers: Value,
    /// 没有是 null
    params: Value,
    /// JSON 的嵌套成对象，别的是字符串，没有是 null
    body: Value,
}

/// 响应
#[derive(Serialize)]
struct DebugResponse {
    headers: Value,
    /// JSON 的嵌套成对象，别的是字符串；没读 body（`BodyStream`）是 null
    body: Value,
}

/// `http call start`
#[derive(Serialize)]
struct DebugStart<'a> {
    method: &'a str,
    /// 真正发出去的地址，params 不打码
    url: &'a str,
    request: &'a DebugRequest,
}

/// 结束那条：success / non-2xx / decode failed / failed 共用这一个形状
#[derive(Serialize)]
struct DebugEnd<'a> {
    method: &'a str,
    url: &'a str,
    /// 传输失败（没拿到响应头）时是 null
    status: Option<u16>,
    elapsed_ms: u128,
    request: &'a DebugRequest,
    /// 传输失败时是 null
    response: Option<DebugResponse>,
    /// 按 `{:#}` 带 source 链；成功、非 2xx 时是 null
    error: Option<String>,
}

/// 消息后面接 JSON：`pretty` 时 JSON 另起一行、缩进成多行，否则整条一行。
/// `level` 为 `None` 时是 INFO（start / success），失败时按配置的级别。序列化失败返回 `JsonError`，不打日志
fn log_debug(
    level: Option<FailedLogLevel>,
    message: &str,
    pretty: bool,
    fields: &impl Serialize,
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

// ---------------------------- JSON 值 ----------------------------

/// 头转成 JSON 对象，同名多值是数组，按 `HeaderMap` 的顺序。
/// 不是 UTF-8 的值写成 `"<N bytes>"`。不打码，sensitive 的头也是原值
fn headers_json(headers: &HeaderMap) -> Value {
    let value_json = |v: &HeaderValue| match std::str::from_utf8(v.as_bytes()) {
        Ok(s) => Value::from(s),
        Err(_) => Value::from(format!("<{} bytes>", v.len())),
    };
    let map = headers
        .keys()
        .map(|name| {
            let mut values: Vec<_> = headers.get_all(name).iter().map(value_json).collect();
            let value = match values.len() {
                1 => values.remove(0),
                _ => Value::Array(values),
            };
            (name.to_string(), value)
        })
        .collect();
    Value::Object(map)
}

/// 文本能解析成 JSON 就作为嵌套的 JSON 值，否则原样作为字符串
fn text_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::from(text))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use test_support::headers::header_map;
    use test_support::logs::capture;

    use super::*;

    /// 开关和 client 级默认头
    mod state {
        use super::*;

        /// 默认关，`.debug(true)` 打开，build 后带到 ReqwestClient 上；pretty 默认关
        #[test]
        fn switch_reaches_client() {
            assert_eq!(ReqwestConfig::default().build().unwrap().debug.mode(), None);
            let client = ReqwestConfig::default().debug(true).build().unwrap();
            assert_eq!(client.debug.mode(), Some(false));
        }

        /// `ReqwestClient::debug()`：默认配置加上 debug
        #[test]
        fn shortcut_is_default_with_debug() {
            let client = ReqwestClient::debug().unwrap();
            assert_eq!(client.debug.mode(), Some(false));
            assert_eq!(
                client.debug.default_headers,
                header_map(&[("accept", "*/*")])
            );
        }

        /// set_debug / set_pretty 原地改，所有 clone 一起变；debug 关时 pretty 不起作用
        #[test]
        fn switches_are_shared_by_clones() {
            let client = ReqwestConfig::default().build().unwrap();
            let clone = client.clone();
            client.set_pretty(true);
            assert_eq!(clone.debug.mode(), None);
            client.set_debug(true);
            assert_eq!(clone.debug.mode(), Some(true));
            clone.set_pretty(false);
            assert_eq!(client.debug.mode(), Some(false));
            clone.set_debug(false);
            assert_eq!(client.debug.mode(), None);
        }

        /// 配置文件里写 `debug = true`
        #[test]
        fn parsed_from_config_file() {
            let config: ReqwestConfig = toml::from_str("debug = true").unwrap();
            assert!(config.debug);
        }

        /// 什么都不配时只有 reqwest 默认的 `Accept: */*`
        #[test]
        fn default_headers_start_with_accept() {
            let client = ReqwestConfig::default().build().unwrap();
            assert_eq!(
                client.debug.default_headers,
                header_map(&[("accept", "*/*")])
            );
        }

        /// UA 和 default_headers 都算进来；default_headers 里的同名头覆盖 Accept 和 UA，同名多值只剩最后一个
        #[test]
        fn default_headers_follow_reqwest_rules() {
            let mut configured = header_map(&[("accept", "text/plain"), ("x-a", "1")]);
            configured.append("x-a", "2".parse().unwrap());
            let client = ReqwestConfig {
                user_agent: Some("ua".into()),
                default_headers: configured.into(),
                ..ReqwestConfig::default()
            }
            .build()
            .unwrap();
            assert_eq!(
                client.debug.default_headers,
                header_map(&[("accept", "text/plain"), ("user-agent", "ua"), ("x-a", "2")])
            );

            let client = ReqwestConfig {
                user_agent: Some("ua".into()),
                default_headers: header_map(&[("user-agent", "override")]).into(),
                ..ReqwestConfig::default()
            }
            .build()
            .unwrap();
            assert_eq!(client.debug.default_headers["user-agent"], "override");
        }
    }

    /// `log_debug`：消息后面接 JSON，字段按结构体的顺序，级别按参数
    mod log_debug {
        use super::*;

        fn request() -> DebugRequest {
            DebugRequest {
                headers: json!({"accept": "*/*"}),
                params: Value::Null,
                body: json!({"a": 1}),
            }
        }

        /// pretty：JSON 另起一行、缩进
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
            assert!(err.is(BaseErr::JsonError), "{err:#}");
            assert_eq!(out.text(), "");
        }
    }

    /// `headers_json()` / `text_json()`
    mod json_values {
        use super::*;

        fn value(bytes: &[u8]) -> HeaderValue {
            HeaderValue::from_bytes(bytes).unwrap()
        }

        /// 单值是字符串，同名多值是数组；sensitive 的也是原值；值里的逗号不影响
        #[test]
        fn headers_single_and_multiple_values() {
            let mut h = HeaderMap::new();
            let mut auth = value(b"Bearer t");
            auth.set_sensitive(true);
            h.insert("authorization", auth);
            h.insert("accept", value(b"application/json, */*"));
            h.append("x-a", value(b"1"));
            h.append("x-a", value(b"2"));
            assert_eq!(
                headers_json(&h),
                json!({"authorization": "Bearer t", "accept": "application/json, */*", "x-a": ["1", "2"]})
            );
        }

        /// UTF-8 的值原样，不是 UTF-8 的只给字节数；空的是 `{}`
        #[test]
        fn headers_non_ascii_and_empty() {
            let mut h = HeaderMap::new();
            h.insert("x-zh", value("中文".as_bytes()));
            h.insert("x-bin", value(&[0xff, 0xfe]));
            assert_eq!(
                headers_json(&h),
                json!({"x-zh": "中文", "x-bin": "<2 bytes>"})
            );
            assert_eq!(headers_json(&HeaderMap::new()), json!({}));
        }

        /// 能解析成 JSON 的嵌套进去，否则是字符串（换行原样保留，输出时由 JSON 转义）
        #[test]
        fn text_is_nested_or_string() {
            assert_eq!(text_json(r#"{"a": [1, 2]}"#), json!({"a": [1, 2]}));
            assert_eq!(text_json("<html>\nerr</html>"), json!("<html>\nerr</html>"));
        }
    }
}
