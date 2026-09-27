//! 请求体：[`IntoBody`] 和现成的实现。内存里的 `()` / [`Json`] / [`Form`] / [`Raw`] / [`Multipart`] 是 `Clone`，可以重试；
//! 流式的 [`RawStream`] / `multipart::Form` 不能

use std::fmt;

use reqwest::header::CONTENT_TYPE;

use hygiea_core::{HyErr, redact};

use super::error::invalid_header;
use super::text::{decode_charset, is_text_content_type, to_one_line};
use super::{Body, Bytes, RequestBuilder, multipart};

/// 裸 body 支持的 content-type。不开放任意字符串，要加类型就在这里加变体。
///
/// `json` / `form` / `multipart` 不在这里——它们不只是换个头，body 的编码方式也不同，
/// 由 [`Json`] / [`Form`] / `multipart::Form` 表示，content-type 由 reqwest 自动设。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    /// `application/x-ndjson`，一行一个 JSON 对象的流式格式
    Ndjson,
    /// `application/xml`，RFC 7303 里机器处理 XML 的首选，默认 UTF-8
    Xml,
    /// `text/xml`，历史遗留、默认字符集 us-ascii，SOAP 1.1 用的是这个。和 `Xml` 不能互换
    XmlText,
    /// `text/csv`
    Csv,
    /// `text/plain`
    Plain,
    /// `application/x-protobuf`。也有服务端用 `application/protobuf`，不通就换那个
    Protobuf,
    /// `application/octet-stream`，类型不明的二进制兜底
    OctetStream,
}

impl ContentType {
    /// 对应的 MIME 字符串，就是请求头里 `Content-Type` 的值
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ndjson => "application/x-ndjson",
            Self::Xml => "application/xml",
            Self::XmlText => "text/xml",
            Self::Csv => "text/csv",
            Self::Plain => "text/plain",
            Self::Protobuf => "application/x-protobuf",
            Self::OctetStream => "application/octet-stream",
        }
    }
}

/// 请求体。[`RequestConfig::new`](super::RequestConfig::new) 的 `body` 接受任何实现了它的类型，
/// 按具体类型静态分派，不经过 `serde_json::Value` 中转，序列化推迟到发送时由 reqwest 完成。
///
/// 现成的实现分两类，区别是内容在不在内存里：
///
/// | 内容都在内存里（`Clone`，可以重试） | 流式来源（不能 `Clone`，不能重试） |
/// |---|---|
/// | `()`（没有 body）、[`Json`]、[`Form`]、[`Raw`]、[`Multipart`] | [`RawStream`]、`multipart::Form`（reqwest 原生，段可以是流或文件） |
///
/// 带重试发送（见 [`RetryCtx`](super::RetryCtx)）要求 body 是 `Clone`：每次重试前留一份，发出去的那份被消费掉。
/// 流读一次就没了，所以流式来源不实现 `Clone`，开重试时编译期就报错
pub trait IntoBody {
    /// 把 body 铺到 `RequestBuilder` 上，content-type 一并设置。
    /// 构造 body 本身失败时（比如 [`Multipart`] 某段的 mime 不合法）返回 `Err`，请求不发出去
    fn apply(self, req: RequestBuilder) -> Result<RequestBuilder, HyErr>;

    /// 打日志用的请求体摘要，`None` 表示没有 body。内容不做长度限制——批量导出这类场景由调用方
    /// 自己用 [`RequestConfig::enable_logging`](super::RequestConfig::enable_logging) 控制要不要打。
    /// 序列化失败时返回 `Err`
    fn preview(&self) -> Result<Option<String>, HyErr>;
}

/// 对应 `.json()`，由 reqwest 序列化并自动补 content-type。可以传引用：`Json(&req)`。
/// 日志里按 [`redact::to_redacted_json`] 打，`T` 里标了 `#[redact(..)]` 的字段会打码
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Json<T>(pub T);

/// 对应 `.form()`，由 reqwest（serde_urlencoded）编码并自动设 content-type。
/// 支持结构体、map 和键值对序列，`None` 字段省略；嵌套值不支持，错误在发送时返回。
/// 日志里按 [`redact::to_redacted_json`] 打，`T` 里标了 `#[redact(..)]` 的字段会打码；打出来是 JSON 形式，
/// 不是实际发出的 urlencoded
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Form<T>(pub T);

/// 内存里的原始字节，对应 `.body()`。`Bytes` 可以从 `String` / `Vec<u8>` / `&'static str` /
/// `&'static [u8]` 转来，clone 只是引用计数加一。content-type 按 [`ContentType`] 一并带上，不开放任意取值。
/// 流式来源（文件、`Body::wrap_stream`）用 [`RawStream`]
#[derive(Debug, Clone)]
pub struct Raw(pub ContentType, pub Bytes);

impl Raw {
    /// `Raw(content_type, body.into())` 的简写
    pub fn new(content_type: ContentType, body: impl Into<Bytes>) -> Self {
        Self(content_type, body.into())
    }
}

/// 流式来源的原始 body，对应 `.body()`：`tokio::fs::File`、`Body::wrap_stream` 包的流等，
/// 边读边发，不整个读进内存。读一次就没了，所以不能重试；内容在内存里时用 [`Raw`]
#[derive(Debug)]
pub struct RawStream(pub ContentType, pub Body);

impl RawStream {
    /// `RawStream(content_type, body.into())` 的简写
    pub fn new(content_type: ContentType, body: impl Into<Body>) -> Self {
        Self(content_type, body.into())
    }
}

/// 内容都在内存里的 `multipart/form-data`，每段是文本或字节（可以带文件名和 mime），可以重试。
/// 每次发送时转成 reqwest 的 `multipart::Form`，boundary 和各段头由 reqwest 生成。
/// 有段要从文件、流里边读边发时，直接用 reqwest 原生的 `multipart::Form`（不能重试）
///
/// ```ignore
/// let body = Multipart::new()
///     .text("user", "alice")
///     .part("avatar", MultipartPart::bytes(png).file_name("a.png").mime("image/png"));
/// ```
#[derive(Debug, Clone, Default)]
pub struct Multipart {
    parts: Vec<(String, MultipartPart)>,
}

impl Multipart {
    pub fn new() -> Self {
        Self::default()
    }

    /// 加一个文本段，`MultipartPart::text` 的简写
    pub fn text(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.part(name, MultipartPart::text(value))
    }

    /// 加一段，按加入顺序发出；同名的段可以有多个
    pub fn part(mut self, name: impl Into<String>, part: MultipartPart) -> Self {
        self.parts.push((name.into(), part));
        self
    }
}

/// [`Multipart`] 的一段。`Debug` 只显示字节数，不显示内容
#[derive(Clone)]
pub struct MultipartPart {
    content: Bytes,
    file_name: Option<String>,
    mime: Option<String>,
}

impl MultipartPart {
    /// 文本段
    pub fn text(value: impl Into<String>) -> Self {
        Self::bytes(value.into())
    }

    /// 字节段，比如文件内容
    pub fn bytes(content: impl Into<Bytes>) -> Self {
        Self {
            content: content.into(),
            file_name: None,
            mime: None,
        }
    }

    /// 这一段的文件名（`Content-Disposition` 里的 `filename`）
    pub fn file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }

    /// 这一段的 `Content-Type`。发送时才由 reqwest 解析，不合法报
    /// [`InvalidHeader`](super::BaseHttpErr::InvalidHeader)，请求不发出去
    pub fn mime(mut self, mime: impl Into<String>) -> Self {
        self.mime = Some(mime.into());
        self
    }
}

impl fmt::Debug for MultipartPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MultipartPart")
            .field("len", &self.content.len())
            .field("file_name", &self.file_name)
            .field("mime", &self.mime)
            .finish()
    }
}

impl IntoBody for () {
    fn apply(self, req: RequestBuilder) -> Result<RequestBuilder, HyErr> {
        Ok(req)
    }

    fn preview(&self) -> Result<Option<String>, HyErr> {
        Ok(None)
    }
}

impl<T: serde::Serialize> IntoBody for Json<T> {
    fn apply(self, req: RequestBuilder) -> Result<RequestBuilder, HyErr> {
        Ok(req.json(&self.0))
    }

    fn preview(&self) -> Result<Option<String>, HyErr> {
        redact::to_redacted_json(&self.0).map(Some)
    }
}

impl<T: serde::Serialize> IntoBody for Form<T> {
    fn apply(self, req: RequestBuilder) -> Result<RequestBuilder, HyErr> {
        Ok(req.form(&self.0))
    }

    fn preview(&self) -> Result<Option<String>, HyErr> {
        redact::to_redacted_json(&self.0).map(Some)
    }
}

impl IntoBody for Raw {
    fn apply(self, req: RequestBuilder) -> Result<RequestBuilder, HyErr> {
        Ok(req.header(CONTENT_TYPE, self.0.as_str()).body(self.1))
    }

    fn preview(&self) -> Result<Option<String>, HyErr> {
        Ok(Some(raw_preview(self.0, &self.1)))
    }
}

impl IntoBody for RawStream {
    fn apply(self, req: RequestBuilder) -> Result<RequestBuilder, HyErr> {
        Ok(req.header(CONTENT_TYPE, self.0.as_str()).body(self.1))
    }

    /// 流读一次就消费掉，拿不到内容，只给一个标记；`Body` 其实是内存字节时和 [`Raw`] 一样打
    fn preview(&self) -> Result<Option<String>, HyErr> {
        Ok(Some(match self.1.as_bytes() {
            Some(bytes) => raw_preview(self.0, bytes),
            None => "<stream>".to_string(),
        }))
    }
}

/// 原始字节的日志摘要：文本打原文（控制字符转义后，和响应日志一样，避免一条日志被拆成多行）；
/// 二进制（protobuf、octet-stream）打出来是乱码，只报字节数
fn raw_preview(content_type: ContentType, bytes: &[u8]) -> String {
    if is_text_content_type(content_type.as_str()) {
        to_one_line(decode_charset(content_type.as_str(), bytes)).into_owned()
    } else {
        format!("<{} bytes {}>", bytes.len(), content_type.as_str())
    }
}

impl IntoBody for Multipart {
    fn apply(self, req: RequestBuilder) -> Result<RequestBuilder, HyErr> {
        let mut form = multipart::Form::new();
        for (name, part) in self.parts {
            // Body::from(Bytes) 是内存字节，不复制；带上长度，reqwest 能算出 Content-Length
            let len = part.content.len() as u64;
            let mut native = multipart::Part::stream_with_length(Body::from(part.content), len);
            if let Some(file_name) = part.file_name {
                native = native.file_name(file_name);
            }
            if let Some(mime) = part.mime {
                native = native.mime_str(&mime).map_err(|e| {
                    invalid_header(format!("invalid mime of multipart part {name}: {mime}"))
                        .with_source(e)
                })?;
            }
            form = form.part(name, native);
        }
        Ok(req.multipart(form))
    }

    /// 每段只打名字、文件名和字节数，不打内容：可能是文件，也可能是密码之类的文本字段
    fn preview(&self) -> Result<Option<String>, HyErr> {
        let parts: Vec<String> = self
            .parts
            .iter()
            .map(|(name, part)| match &part.file_name {
                Some(file_name) => format!("{name}({file_name}, {} bytes)", part.content.len()),
                None => format!("{name}({} bytes)", part.content.len()),
            })
            .collect();
        Ok(Some(format!("<multipart: {}>", parts.join(", "))))
    }
}

/// reqwest 原生的 multipart，段可以是流或文件（`Part::stream`、`Form::file`），不能重试。
/// boundary 和各段头由 reqwest 生成
impl IntoBody for multipart::Form {
    fn apply(self, req: RequestBuilder) -> Result<RequestBuilder, HyErr> {
        Ok(req.multipart(self))
    }

    fn preview(&self) -> Result<Option<String>, HyErr> {
        Ok(Some("<multipart>".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use crate::reqwest_client::Client;
    use hygiea_core::BaseErr;
    use hygiea_core::redact::redact;
    use hygiea_test_support::Unserializable;
    use serde::Serialize;
    use serde_json::{Value, json};

    use super::*;

    #[redact]
    #[derive(Serialize)]
    struct Login {
        user: &'static str,
        #[redact(mask)]
        password: &'static str,
    }

    const LOGIN: Login = Login {
        user: "alice",
        password: "p@ss",
    };

    /// 把 body 铺到一个 POST 请求上并构建出来，看真正要发出去的东西
    fn build(body: impl IntoBody) -> reqwest::Request {
        body.apply(Client::new().post("http://127.0.0.1/"))
            .unwrap()
            .build()
            .unwrap()
    }

    fn content_type(req: &reqwest::Request) -> Option<&str> {
        req.headers().get(CONTENT_TYPE).map(|v| v.to_str().unwrap())
    }

    fn body_bytes(req: &reqwest::Request) -> &[u8] {
        req.body().unwrap().as_bytes().unwrap()
    }

    /// `ContentType` 本身
    mod content_type {
        use super::*;
        use ContentType::*;

        /// 每个变体对应的 MIME 字符串
        #[test]
        fn as_str_maps_every_variant() {
            let expected = [
                (Ndjson, "application/x-ndjson"),
                (Xml, "application/xml"),
                (XmlText, "text/xml"),
                (Csv, "text/csv"),
                (Plain, "text/plain"),
                (Protobuf, "application/x-protobuf"),
                (OctetStream, "application/octet-stream"),
            ];
            for (ct, mime) in expected {
                assert_eq!(ct.as_str(), mime);
            }
        }

        /// 每个取值按 is_text_content_type 的判断：文本类打内容，二进制类只打字节数
        #[test]
        fn text_and_binary_split() {
            for ct in [Ndjson, Xml, XmlText, Csv, Plain] {
                assert!(is_text_content_type(ct.as_str()), "{ct:?}");
            }
            for ct in [Protobuf, OctetStream] {
                assert!(!is_text_content_type(ct.as_str()), "{ct:?}");
            }
        }
    }

    /// `apply()`：真正发出去的 body 和 content-type
    mod apply {
        use super::*;

        /// `()` 什么都不加
        #[test]
        fn unit_adds_nothing() {
            let req = build(());
            assert!(req.body().is_none());
            assert_eq!(content_type(&req), None);
        }

        /// `Json` 发原文（不打码），content-type 由 reqwest 设成 application/json
        #[test]
        fn json_sends_real_content() {
            let req = build(Json(&LOGIN));
            assert_eq!(content_type(&req), Some("application/json"));
            assert_eq!(
                serde_json::from_str::<Value>(std::str::from_utf8(body_bytes(&req)).unwrap())
                    .unwrap(),
                json!({"user":"alice","password":"p@ss"})
            );
        }

        /// `Form` 按 urlencoded 编码
        #[test]
        fn form_is_urlencoded() {
            let req = build(Form(&LOGIN));
            assert_eq!(
                content_type(&req),
                Some("application/x-www-form-urlencoded")
            );
            assert_eq!(body_bytes(&req), b"user=alice&password=p%40ss");
        }

        /// `Raw` 带上声明的 content-type，body 原样
        #[test]
        fn raw_uses_declared_content_type() {
            for ct in [ContentType::Csv, ContentType::Protobuf] {
                let req = build(Raw::new(ct, "a,b"));
                assert_eq!(content_type(&req), Some(ct.as_str()));
                assert_eq!(body_bytes(&req), b"a,b");
            }
        }

        /// multipart 的 boundary 由 reqwest 生成
        #[test]
        fn multipart_sets_boundary() {
            let form = multipart::Form::new().text("k", "v");
            let req = build(form);
            let ct = content_type(&req).unwrap();
            assert!(ct.starts_with("multipart/form-data; boundary="), "{ct}");
        }

        /// 序列化失败不会 panic，错误留到 build() 时由 reqwest 返回
        #[test]
        fn serialize_error_surfaces_at_build() {
            let result = Json(Unserializable)
                .apply(Client::new().post("http://127.0.0.1/"))
                .unwrap()
                .build();
            assert!(result.is_err());
        }
    }

    /// `preview()`：日志里打的摘要
    mod preview {
        use super::*;

        /// 没有 body 就是 `None`，日志里不出现 body 段
        #[test]
        fn unit_is_none() {
            assert_eq!(().preview().unwrap(), None);
        }

        /// `Json` 的预览按 redact 打码，和实际发送的内容不同
        #[test]
        fn json_masks_marked_fields() {
            let preview = Json(&LOGIN).preview().unwrap().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&preview).unwrap(),
                json!({"user":"alice","password":"***"})
            );
        }

        /// `Form` 的预览是 JSON 形式（便于阅读），不是 urlencoded，同样打码
        #[test]
        fn form_previews_as_masked_json() {
            let preview = Form(&LOGIN).preview().unwrap().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&preview).unwrap(),
                json!({"user":"alice","password":"***"})
            );
        }

        /// 序列化失败时预览返回 `Err`，由 send 传给调用方
        #[test]
        fn serialize_error_is_err() {
            let err = Json(Unserializable).preview().unwrap_err();
            assert!(err.is(BaseErr::JsonError));
        }

        /// 文本类 `Raw` 打原文，换行换成空格，不会把一条日志拆成多行
        #[test]
        fn raw_text_is_shown_on_one_line() {
            let preview = Raw::new(ContentType::Csv, "a,b\nc,d").preview().unwrap();
            assert_eq!(preview.as_deref(), Some("a,b c,d"));
        }

        /// 声明是文本但不是合法 UTF-8，非法字节替换成 `�`
        #[test]
        fn raw_invalid_utf8_text_is_lossy() {
            let preview = Raw::new(ContentType::Plain, vec![b'a', 0xff, b'b'])
                .preview()
                .unwrap();
            assert_eq!(preview.as_deref(), Some("a\u{fffd}b"));
        }

        /// 二进制类只打字节数和类型
        #[test]
        fn raw_binary_shows_size_only() {
            let preview = Raw::new(ContentType::Protobuf, vec![1u8, 2, 3])
                .preview()
                .unwrap();
            assert_eq!(preview.as_deref(), Some("<3 bytes application/x-protobuf>"));
        }

        /// 流式 body 读一次就没了，只打一个标记
        #[test]
        fn raw_stream_shows_marker() {
            let chunks = vec![Ok::<_, std::io::Error>(Bytes::from_static(b"x"))];
            let body = Body::wrap_stream(futures_util::stream::iter(chunks));
            let preview = RawStream(ContentType::OctetStream, body).preview().unwrap();
            assert_eq!(preview.as_deref(), Some("<stream>"));
        }

        /// `RawStream` 里其实是内存字节时，和 `Raw` 一样打内容
        #[test]
        fn raw_stream_with_bytes_shows_content() {
            let preview = RawStream::new(ContentType::Plain, "abc").preview().unwrap();
            assert_eq!(preview.as_deref(), Some("abc"));
        }

        /// `Multipart` 每段只打名字、文件名和字节数，不打内容
        #[test]
        fn multipart_shows_names_and_sizes() {
            let body = Multipart::new().text("password", "p@ss").part(
                "avatar",
                MultipartPart::bytes(vec![0u8; 10]).file_name("a.png"),
            );
            let preview = body.preview().unwrap().unwrap();
            assert_eq!(
                preview,
                "<multipart: password(4 bytes), avatar(a.png, 10 bytes)>"
            );
        }

        /// multipart 不打内容（可能是文件）
        #[test]
        fn multipart_shows_marker() {
            let preview = multipart::Form::new().text("k", "v").preview().unwrap();
            assert_eq!(preview.as_deref(), Some("<multipart>"));
        }
    }
}
