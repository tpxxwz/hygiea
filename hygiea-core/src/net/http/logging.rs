//! 日志：失败日志级别，以及 send 用的预览和日志工具。打码见 [`crate::redact`]
//!
//! ## 字节转成文本的几个函数
//!
//! | 函数 | 作用 | 实现 |
//! |---|---|---|
//! | `is_text_content_type(ct)` | 这个 content-type 算不算文本 | 本模块，按 MIME 类型判断 |
//! | `decode_charset(ct, bytes)` | 按 ct 里的 `charset` 把字节解码成文本，默认 UTF-8 | 本模块，解码用 `encoding_rs` |
//! | `body_text(headers, body)` | 响应 body 转文本：文本类解码，二进制只给 `<N bytes ..>`；**不转义** | 本模块 |
//! | `to_one_line(text)` | 让日志保持一行：`\n` `\r` `\t` 换成空格，其他控制字符转义 | 本模块，单字符转义用 `char::escape_default` |
//! | `body_preview(headers, body)` | 响应的日志摘要，就是 `to_one_line(body_text(..))` | 本模块 |
//!
//! 标准库的 `str::escape_default` 会转义所有非 ASCII（中文变成 `\u{..}`），`str::escape_debug` 会转义
//! 引号和反斜杠（JSON 里全是 `\"`），都不适合日志，所以 `to_one_line` 自己实现。
//! 打码用的 `redact::to_redacted_json` 在 [`crate::redact`]。
//!
//! ## 调用链
//!
//! ```text
//! 请求（日志 req=）
//!   send ─ req_preview
//!     ├─ params → redact::to_redacted_json（打码）
//!     └─ body   → IntoBody::preview()                      (parts.rs)
//!           ├─ Json / Form → redact::to_redacted_json（打码）
//!           └─ Raw → is_text_content_type(ContentType::as_str())
//!                 ├─ 文本   → to_one_line(decode_charset(..))
//!                 └─ 二进制 → "<N bytes type>"
//!
//! 响应（send 拿到 body 之后）
//!   ├─ 非 2xx   日志 resp= body_preview       错误 body  body_text（不转义，给调用方解析）
//!   ├─ 解码失败 日志 resp= body_preview
//!   └─ 成功     日志 resp= FromBody::decoded_preview 有值（Json<T> 打码、String 原文）→ to_one_line(..)
//!                          没有（Bytes / ()）                                     → 不输出 resp 字段
//!
//! body_preview = to_one_line ∘ body_text
//! body_text    = is_text_content_type ? decode_charset : "<N bytes ..>"
//! ```
//!
//! 规则：给人看的（日志 `req=` / `resp=`）先打码、再转文本、最后 `to_one_line`；成功响应只有类型能打码
//! （提供了 `decoded_preview`）才打内容，否则不输出 `resp`；非 2xx 和解码失败打原文，排查问题要看对方回了什么。
//! 给程序用的（非 2xx 错误里的 `body`）只 `body_text`，内容和原文一致。
//! 是不是文本一律 `is_text_content_type`，字节转文本一律 `decode_charset`

use std::borrow::Cow;
use std::time::Instant;

use reqwest::header::CONTENT_TYPE;

use crate::HyErr;

use super::error::request_failed;
use super::{Bytes, HeaderMap, HttpResponse, Level, Method, StatusCode, Url};

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

// ---------------- 字节转成日志 / 错误里的文本 ----------------
//
// 调用方在前、被调用的在后：body_preview → body_text → is_text_content_type / decode_charset，
// to_one_line 是所有日志摘要的最后一步

/// 日志里的响应摘要：[`body_text`] 再转义控制字符（换行、`\x1b` 终端控制序列等），
/// 避免一条日志被拆成多行，或者被响应内容伪造出假的日志行
pub(super) fn body_preview<'a>(headers: &HeaderMap, body: &'a Bytes) -> Cow<'a, str> {
    to_one_line(body_text(headers, body))
}

/// 响应 body 的文本形式，日志预览和非 2xx 错误里的 body 共用。先看 `Content-Type`：
/// - 文本类：按其中的 `charset` 解码（比如 GBK、Shift_JIS），没写或不认识就按 UTF-8，非法字节替换成 `�`
/// - 其他：二进制，只给 `<N bytes type>`，原文打出来是乱码
/// - 没有 `Content-Type`：能按 UTF-8 解码就用原文，否则只给 `<N bytes>`
///
/// 不转义控制字符，内容和原文一致，错误里的 body 要能被调用方拿去解析
pub(super) fn body_text<'a>(headers: &HeaderMap, body: &'a Bytes) -> Cow<'a, str> {
    let content_type = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok());
    let text = match content_type {
        Some(ct) if !is_text_content_type(ct) => None,
        Some(ct) => Some(decode_charset(ct, body)),
        None => std::str::from_utf8(body).ok().map(Into::into),
    };
    text.unwrap_or_else(|| match content_type {
        Some(ct) => format!("<{} bytes {ct}>", body.len()).into(),
        None => format!("<{} bytes>", body.len()).into(),
    })
}

/// 什么 content-type 算文本，请求的 `Raw` 预览和响应日志共用这一套规则：
/// `text/*`（HTML 错误页、纯文本、CSV……）、JSON、NDJSON、XML、表单、JavaScript、YAML、GraphQL，
/// 以及 `+json` / `+xml` 后缀的变体（`application/problem+json`、`application/vnd.api+json`、
/// `application/soap+xml`……）
pub(super) fn is_text_content_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    mime.starts_with("text/")
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/x-ndjson"
                | "application/xml"
                | "application/x-www-form-urlencoded"
                | "application/javascript"
                | "application/x-javascript"
                | "application/ecmascript"
                | "application/yaml"
                | "application/x-yaml"
                | "application/graphql"
        )
}

/// 按 `Content-Type` 里的 `charset` 解码，没写或不认识就按 UTF-8；开头的 BOM 去掉。UTF-8 且合法时不分配
pub(super) fn decode_charset<'a>(content_type: &str, body: &'a [u8]) -> Cow<'a, str> {
    let encoding = content_type
        .split(';')
        .skip(1)
        .filter_map(|param| param.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
        .and_then(|(_, value)| {
            encoding_rs::Encoding::for_label(value.trim().trim_matches('"').as_bytes())
        })
        .unwrap_or(encoding_rs::UTF_8);
    // encoding_rs 的 decode 会识别并去掉开头的 BOM，有 BOM 时以 BOM 表示的编码为准（WHATWG 的做法）
    encoding.decode(body).0
}

/// 让日志保持一行：换行、回车、制表符换成空格，其他控制字符（比如 `\x1b` 终端控制序列）转义成
/// `\u{1b}` 这类形式，其余原样；没有控制字符时不分配。
///
/// 这三个在 JSON 里只可能是字段之间的空白（字符串里的换行必须写成 `\n`，原始换行不是合法 JSON），
/// 换成空格后从日志复制出来的 JSON 照样能反序列化，值和原文一样
pub(super) fn to_one_line(text: Cow<'_, str>) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return text;
    }
    let mut escaped = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\n' | '\r' | '\t' => escaped.push(' '),
            c if c.is_control() => escaped.extend(c.escape_default()),
            c => escaped.push(c),
        }
    }
    escaped.into()
}

// ---------------- 发出之后的上下文（send 用）----------------

/// 请求发出之后，读 body 期间要用的上下文：发送 / 读 body 失败、解码失败时打日志。
/// send 建好之后交给 [`RespBody`](super::RespBody)，`FromBody` 的默认实现也靠它打解码失败日志
pub(super) struct SendCtx {
    pub(super) method: Method,
    /// 打码后的地址
    pub(super) url: Url,
    pub(super) req_preview: String,
    pub(super) failed_log_level: FailedLogLevel,
    /// 请求发出的时刻，从这里算耗时
    pub(super) sent_at: Instant,
}

impl SendCtx {
    /// 发出之后的传输失败（发送、读 body）：包成 BaseHttpErr::RequestFailed、打一条失败日志。
    /// 读 body 时已经有状态码，发送失败时没有
    pub(super) fn transport_failed(&self, e: reqwest::Error, status: Option<StatusCode>) -> HyErr {
        let err = request_failed(&self.method, self.url.as_str(), e);
        let failure = Failure {
            status,
            elapsed_ms: Some(self.sent_at.elapsed().as_millis()),
            error: Some(&err),
            ..Failure::new(&self.method, self.url.as_str(), &self.req_preview)
        };
        log_failed(self.failed_log_level, "http call failed", &failure);
        err
    }

    /// 2xx 但解码失败：打失败日志，带上原文方便对照
    pub(super) fn decode_failed(
        &self,
        status: StatusCode,
        headers: &HeaderMap,
        body: &Bytes,
        err: &HyErr,
    ) {
        let resp = HttpResponse {
            method: self.method.clone(),
            status,
            headers: headers.clone(),
            url: self.url.clone(),
            elapsed: self.sent_at.elapsed(),
            body: (),
        };
        log_decode_failed(
            self.failed_log_level,
            &resp,
            &self.req_preview,
            err,
            &body_preview(headers, body),
        );
    }
}

// ---------------- 拿到响应之后的日志（send 用）----------------
//
// `req` 是请求摘要（`req=` 那一段），`resp_preview` 是响应摘要

/// 成功。调用前自己判断 enable_logging，省得关了日志还去算预览
pub(super) fn log_resp_success<Resp>(
    resp: &HttpResponse<Resp>,
    req: &str,
    resp_preview: Option<&str>,
) {
    log_success(&Success {
        method: &resp.method,
        url: resp.url.as_str(),
        status: resp.status,
        elapsed_ms: resp.elapsed.as_millis(),
        req,
        resp: resp_preview,
    });
}

/// 非 2xx，不管 enable_logging 都打
pub(super) fn log_status_failed<Resp>(
    level: FailedLogLevel,
    resp: &HttpResponse<Resp>,
    req: &str,
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
    req: &str,
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
fn failure_of<'a, Resp>(resp: &'a HttpResponse<Resp>, req: &'a str) -> Failure<'a> {
    Failure {
        status: Some(resp.status),
        elapsed_ms: Some(resp.elapsed.as_millis()),
        ..Failure::new(&resp.method, resp.url.as_str(), req)
    }
}

// ---------------- 三种日志：start / success / failed ----------------
//
// 每种一个字段结构体加一个入口函数，字段同名同序（method、url、status、elapsed_ms、req、resp），
// 日志平台上可以按同一套字段检索。打日志只走这三个入口

/// 请求发出之前的那条日志（INFO）
pub(super) struct Start<'a> {
    pub(super) method: &'a Method,
    /// 打码后的地址
    pub(super) url: &'a str,
    pub(super) req: &'a str,
}

/// 打 `http call start`
pub(super) fn log_start(s: &Start<'_>) {
    tracing::info!(method = %s.method, url = %s.url, req = %s.req, "http call start");
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
    pub(super) fn new(method: &'a Method, url: &'a str, req: &'a str) -> Self {
        Self {
            method,
            url,
            status: None,
            elapsed_ms: None,
            req,
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hygiea_test_support::logs::capture;

    use crate::net::http::{HeaderValue, Method, StatusCode, Url};
    use crate::{BaseErr, err};

    use super::*;

    fn headers(content_type: Option<&'static str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(ct) = content_type {
            h.insert(CONTENT_TYPE, HeaderValue::from_static(ct));
        }
        h
    }

    fn preview(content_type: Option<&'static str>, body: &[u8]) -> String {
        body_preview(&headers(content_type), &Bytes::copy_from_slice(body)).into_owned()
    }

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

    /// `is_text_content_type()`：请求和响应判断文本的共用规则
    mod is_text_content_type {
        use super::*;

        /// 算文本的：text/*、JSON / XML 及其 `+json` / `+xml` 变体、NDJSON、表单、JavaScript
        #[test]
        fn text_types() {
            for ct in [
                "text/plain",
                "text/html",
                "text/csv",
                "application/json",
                "application/problem+json",
                "application/xml",
                "application/soap+xml",
                "application/x-ndjson",
                "application/x-www-form-urlencoded",
                "application/javascript",
                "application/x-javascript",
                "application/ecmascript",
                "application/yaml",
                "application/x-yaml",
                "application/graphql",
                // 常见的错误页：nginx / 网关的 404、Cloudflare 的 5xx / 1xxx 页面，以及 JSON:API 这类
                "text/html; charset=UTF-8",
                "application/vnd.api+json",
                "application/hal+json",
            ] {
                assert!(is_text_content_type(ct), "{ct}");
            }
        }

        /// 不算文本的：二进制和 multipart，空串也不算
        #[test]
        fn non_text_types() {
            for ct in [
                "application/octet-stream",
                "application/x-protobuf",
                "image/png",
                "multipart/form-data; boundary=x",
                "",
            ] {
                assert!(!is_text_content_type(ct), "{ct}");
            }
        }

        /// 忽略 `; charset=..` 这类参数、前后空白和大小写
        #[test]
        fn ignores_params_whitespace_and_case() {
            assert!(is_text_content_type("Application/JSON; charset=utf-8"));
            assert!(is_text_content_type("  text/plain  "));
        }
    }

    /// `to_one_line()`：防止响应内容把一条日志拆成多行或伪造日志行
    mod to_one_line {
        use super::*;

        /// 没有控制字符时原样借用，不分配
        #[test]
        fn clean_text_is_borrowed() {
            assert!(matches!(
                to_one_line(Cow::Borrowed("abc")),
                Cow::Borrowed("abc")
            ));
        }

        /// 换行、回车、制表符换成空格，ESC 这类其他控制字符转义
        #[test]
        fn escapes_control_chars() {
            let escaped = to_one_line(Cow::Borrowed("a\nb\rc\td\x1b[31m"));
            assert_eq!(escaped, r"a b c d\u{1b}[31m");
        }

        /// 格式化过的多行 JSON 变成一行后仍能反序列化，值和原文一样
        #[test]
        fn pretty_json_stays_valid() {
            let pretty = "{\n\t\"a\": \"x\\ny\",\r\n  \"b\": [1, 2]\n}";
            let line = to_one_line(Cow::Borrowed(pretty));
            assert!(!line.contains('\n'));
            let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(
                parsed,
                serde_json::from_str::<serde_json::Value>(pretty).unwrap()
            );
        }

        /// 普通非 ASCII 字符不转义
        #[test]
        fn keeps_unicode() {
            assert_eq!(to_one_line(Cow::Borrowed("你好 é")), "你好 é");
        }
    }

    /// `body_preview()`：日志里的响应摘要
    mod body_preview {
        use super::*;

        /// 文本类 content-type 打原文，参数和大小写不影响
        #[test]
        fn text_types_show_content() {
            let body = br#"{"a":1}"#;
            assert_eq!(preview(Some("application/json"), body), r#"{"a":1}"#);
            assert_eq!(
                preview(Some("application/json; charset=utf-8"), body),
                r#"{"a":1}"#
            );
            assert_eq!(
                preview(Some("application/problem+json"), body),
                r#"{"a":1}"#
            );
            assert_eq!(preview(Some("TEXT/Plain"), b"hi"), "hi");
        }

        /// 二进制类只打字节数，即使内容恰好是合法 UTF-8
        #[test]
        fn binary_types_show_size_only() {
            assert_eq!(
                preview(Some("application/x-protobuf"), b"\x08\x01"),
                "<2 bytes application/x-protobuf>"
            );
            assert_eq!(
                preview(Some("image/png"), &[0x89, 0x50]),
                "<2 bytes image/png>"
            );
        }

        /// 没有 content-type 时看能不能按 UTF-8 解码
        #[test]
        fn missing_content_type_falls_back_to_utf8_check() {
            assert_eq!(preview(None, b"plain"), "plain");
            assert_eq!(preview(None, &[0xff, 0xfe]), "<2 bytes>");
        }

        /// content-type 的值不是合法文本时，当作没有 content-type 处理
        #[test]
        fn unreadable_content_type_is_ignored() {
            let mut h = HeaderMap::new();
            h.insert(CONTENT_TYPE, HeaderValue::from_bytes(b"\xfftext").unwrap());
            assert_eq!(body_preview(&h, &Bytes::from_static(b"ok")), "ok");
        }

        /// 按 Content-Type 里声明的 charset 解码（"你好" 的 GBK 字节），引号、大小写都认
        #[test]
        fn decodes_declared_charset() {
            let gbk = [0xc4, 0xe3, 0xba, 0xc3];
            assert_eq!(preview(Some("text/html; charset=gbk"), &gbk), "你好");
            assert_eq!(preview(Some(r#"text/plain; Charset="GBK""#), &gbk), "你好");
        }

        /// 开头的 BOM 去掉（error 里的 body 拿去解析时不会被它卡住）
        #[test]
        fn strips_bom() {
            assert_eq!(
                preview(Some("application/json"), b"\xEF\xBB\xBF{\"a\":1}"),
                r#"{"a":1}"#
            );
        }

        /// 没写或不认识的 charset 按 UTF-8，非法字节替换成 `�`
        #[test]
        fn unknown_charset_is_lossy_utf8() {
            for ct in ["text/plain", "text/plain; charset=no-such"] {
                assert_eq!(preview(Some(ct), b"a\xffb"), "a\u{fffd}b", "{ct}");
            }
        }

        /// body_text 不转义：换行原样保留，错误里的 body 拿去解析时和原文一致；预览才转义
        #[test]
        fn body_text_keeps_newlines() {
            let h = headers(Some("application/json"));
            let body = Bytes::from_static(b"{\n  \"a\": 1\n}");
            assert_eq!(body_text(&h, &body), "{\n  \"a\": 1\n}");
            assert_eq!(body_preview(&h, &body), r#"{   "a": 1 }"#);
        }

        /// 原文里的换行换成空格，其他控制字符转义
        #[test]
        fn escapes_control_chars() {
            assert_eq!(
                preview(Some("application/json"), b"{\n  \"a\": 1\n}"),
                r#"{   "a": 1 }"#
            );
            assert_eq!(preview(None, b"x\x1b[31my"), r"x\u{1b}[31my");
        }

        /// 空 body：文本类是空串，二进制类是 0 字节
        #[test]
        fn empty_body() {
            assert_eq!(preview(Some("application/json"), b""), "");
            assert_eq!(
                preview(Some("application/octet-stream"), b""),
                "<0 bytes application/octet-stream>"
            );
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
            let failure = Failure::new(&method, "http://h/", "[]");
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
                    &Failure::new(&method, "http://h/", "[]"),
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
                ..Failure::new(&method, "http://h/x", "[params:1]")
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
                    url: "http://h/x?token=***",
                    req: "[params:1]",
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
            let line = one_line(|| log_resp_success(&resp(), "[params:1]", Some("resp-body")));
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
            let line = one_line(|| {
                log_status_failed(FailedLogLevel::Error, &resp(), "[params:1]", "resp-body")
            });
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
                log_decode_failed(
                    FailedLogLevel::Warn,
                    &resp(),
                    "[params:1]",
                    &err,
                    "resp-body",
                )
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
}
