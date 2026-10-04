//! 字节转成日志 / 错误里的文本：charset 解码、是不是文本、控制字符转义。打码见 [`hygiea_core::redact`]
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
//! | `headers_text(headers)` | debug 日志里的请求头 / 响应头，一行，不打码 | 本模块 |
//!
//! 标准库的 `str::escape_default` 会转义所有非 ASCII（中文变成 `\u{..}`），`str::escape_debug` 会转义
//! 引号和反斜杠（JSON 里全是 `\"`），都不适合日志，所以 `to_one_line` 自己实现。
//! 打码用的 `redact::to_redacted_json` 在 [`hygiea_core::redact`]。
//!
//! ## 调用链
//!
//! ```text
//! 请求（日志 req=）
//!   send ─ req_preview
//!     ├─ params → redact::to_redacted_json（打码）
//!     └─ body   → IntoBody::preview()                      (body.rs)  
//!           ├─ Json / Form → redact::to_redacted_json（打码）
//!           └─ Raw → is_text_content_type(ContentType::as_str())
//!                 ├─ 文本   → to_one_line(decode_charset(..))
//!                 └─ 二进制 → "<N bytes type>"
//!
//! 响应（send 拿到 body 之后）
//!   ├─ 非 2xx   日志 resp= body_preview       错误 body  body_text（不转义，给调用方解析）
//!   ├─ 解码失败 日志 resp= body_preview
//!   └─ 成功     日志 resp= FromBody::resp_preview 有值（Json<T> 打码、String 原文）→ to_one_line(..)
//!                          没有（Bytes / ()）                                     → 不输出 resp 字段
//!
//! body_preview = to_one_line ∘ body_text
//! body_text    = is_text_content_type ? decode_charset : "<N bytes ..>"
//! ```
//!
//! 规则：给人看的（日志 `req=` / `resp=`）先打码、再转文本、最后 `to_one_line`；成功响应只有类型能打码

use std::borrow::Cow;

use reqwest::header::CONTENT_TYPE;

use super::{Bytes, HeaderMap, HeaderValue};

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

/// debug 日志里的头：`{name: value, name: [v1, v2]}`，同名多值合成一个列表，按 `HeaderMap` 的顺序。
/// 不是 UTF-8 的值显示成 `<N bytes>`，最后过一遍 [`to_one_line`]。不打码，sensitive 的头也打原值
#[cfg_attr(not(feature = "debug-log"), allow(dead_code))]
pub(super) fn headers_text(headers: &HeaderMap) -> String {
    let value_text = |v: &HeaderValue| match std::str::from_utf8(v.as_bytes()) {
        Ok(s) => s.to_string(),
        Err(_) => format!("<{} bytes>", v.len()),
    };
    let entries: Vec<String> = headers
        .keys()
        .map(|name| {
            let values: Vec<String> = headers.get_all(name).iter().map(value_text).collect();
            match values.as_slice() {
                [one] => format!("{name}: {one}"),
                many => format!("{name}: [{}]", many.join(", ")),
            }
        })
        .collect();
    to_one_line(format!("{{{}}}", entries.join(", ")).into()).into_owned()
}

#[cfg(test)]
mod tests {
    use crate::reqwest_client::HeaderValue;

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

    /// `headers_text()`：debug 日志里的头
    mod headers_text {
        use super::*;

        fn value(bytes: &[u8]) -> HeaderValue {
            HeaderValue::from_bytes(bytes).unwrap()
        }

        /// 空的是 `{}`
        #[test]
        fn empty() {
            assert_eq!(headers_text(&HeaderMap::new()), "{}");
        }

        /// 单值直接写，同名多值合成列表；sensitive 的也打原值
        #[test]
        fn single_and_multiple_values() {
            let mut h = HeaderMap::new();
            let mut auth = value(b"Bearer t");
            auth.set_sensitive(true);
            h.insert("authorization", auth);
            h.append("x-a", value(b"1"));
            h.append("x-a", value(b"2"));
            assert_eq!(headers_text(&h), "{authorization: Bearer t, x-a: [1, 2]}");
        }

        /// UTF-8 的值原样显示，不是 UTF-8 的只给字节数，制表符换成空格
        #[test]
        fn non_ascii_values() {
            let mut h = HeaderMap::new();
            h.insert("x-zh", value("中文".as_bytes()));
            h.insert("x-bin", value(&[0xff, 0xfe]));
            h.insert("x-tab", value(b"a\tb"));
            assert_eq!(
                headers_text(&h),
                "{x-zh: 中文, x-bin: <2 bytes>, x-tab: a b}"
            );
        }
    }
}
