//! 请求和响应的组成部分：
//! - 请求头 [`IntoHeaders`] 及保留字段检查
//! - 认证 [`Auth`]
//! - 请求体 [`IntoBody`]（现成的 [`Json`] / [`Form`] / [`Raw`] / `multipart::Form`）
//! - 响应体 [`FromBody`]（现成的 `Bytes` / `String` / `Json` / `()` / [`BodyStream`]），读 body 用 [`RespBody`]。`Json` 两个方向都用
//!
//! params 没有单独的类型，就是 `RequestConfig` 上一个 `Serialize` 的泛型字段

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use reqwest::header::CONTENT_TYPE;

use crate::{BaseErr, HyErr, ResultExt, err, redact};

use futures_util::{Stream, StreamExt, stream};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::error::{invalid_header, request_failed, write_failed};
use super::logging::{SendCtx, decode_charset, is_text_content_type, to_one_line};
use super::{Body, Bytes, HeaderMap, HeaderValue, RequestBuilder, StatusCode, multipart};

// ============================ 请求头 ============================

/// 能作为请求头传给 [`RequestConfig::headers`](super::RequestConfig::headers) 的类型，转换失败返回 `HyErr`。
///
/// 内置只实现了 [`HeaderMap`]（同名多值用 `append` 追加）。其他形式由外部类型自己实现这个 trait，
/// 比如签名头的结构体；保留字段统一由 [`RequestConfig::headers`](super::RequestConfig::headers) 拦截
pub trait IntoHeaders {
    fn into_headers(self) -> Result<HeaderMap, HyErr>;
}

/// 已经是合法的 header，直接用
impl IntoHeaders for HeaderMap {
    fn into_headers(self) -> Result<HeaderMap, HyErr> {
        Ok(self)
    }
}

/// 不允许手工设置的 header：要么有专门的入口，要么由 hyper 按实际情况计算。
/// 名字用小写，因为 `HeaderName` 解析后会统一成小写。
const RESERVED_HEADERS: &[&str] = &[
    // 有专门入口：auth（Auth），手设不会被标记 sensitive
    "authorization",
    // 有专门入口：body（IntoBody），根据请求体类型设置；
    // reqwest 的 header 是 append 不是覆盖，手设会多出一条
    "content-type",
    // 以下三个由 hyper 按实际情况计算或管理
    "content-length",
    "transfer-encoding",
    "connection",
];

/// client 和请求两级共用：转换并拦保留字段（`ClientConfig::default_headers`、`RequestConfig::headers`）
pub(super) fn checked_headers(headers: impl IntoHeaders) -> Result<HeaderMap, HyErr> {
    let headers = headers.into_headers()?;
    // 拦掉 RESERVED_HEADERS 里那些
    for name in headers.keys() {
        if RESERVED_HEADERS.contains(&name.as_str()) {
            return Err(invalid_header(format!(
                "{name:?} is reserved, use the dedicated method or leave it to hyper"
            )));
        }
    }
    Ok(headers)
}

// ============================ 认证 ============================

/// Authorization 头。走这里而不是自己 `headers(("Authorization", ..))` 的原因是 reqwest 会把值标成
/// **sensitive**，`HeaderValue` 的 `Debug` 输出变成 `Sensitive`，凭据不会漏进日志；手设的头没有这层保护。
///
/// 跨域重定向时 reqwest 会自动剥掉这个头。`Debug` 输出里凭据显示成 `***`
#[derive(Clone)]
pub enum Auth {
    Basic {
        username: String,
        password: Option<String>,
    },
    Bearer(String),
    /// 自定义 scheme，拼成 `Authorization: <scheme> <credentials>`。
    /// 交易所的 HMAC 签名、AWS 的 `AWS4-HMAC-SHA256` 这类都走这里
    Custom {
        scheme: String,
        credentials: String,
    },
}

/// 手写而不是 derive：derive 出来的 `Debug` 会把 token、密码原样打出来
impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let masked = redact::MASKED;
        match self {
            Self::Basic { username, password } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &password.as_ref().map(|_| masked))
                .finish(),
            Self::Bearer(_) => f.debug_tuple("Bearer").field(&masked).finish(),
            Self::Custom { scheme, .. } => f
                .debug_struct("Custom")
                .field("scheme", scheme)
                .field("credentials", &masked)
                .finish(),
        }
    }
}

/// 构造认证头的值并标成 sensitive，`Debug` 里显示 `Sensitive`，凭据不会漏进日志。
///
/// reqwest 没有暴露 header_sensitive，所以自己构造，再走公开的 `header()`——它只会把非敏感变敏感、
/// 不会反向关掉，标记能保住。值里有换行之类的非法字符时报 `InvalidHeader`，错误信息里不带凭据原文
pub(super) fn auth_value(raw: String) -> Result<HeaderValue, HyErr> {
    let mut value = HeaderValue::from_str(&raw).map_err(|e| {
        invalid_header("authorization value contains invalid characters").with_source(e)
    })?;
    value.set_sensitive(true);
    Ok(value)
}

// ============================ 请求体 ============================

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
/// 现成的实现：[`Json`] / [`Form`] / [`Raw`] / `multipart::Form`，`()` 表示没有 body。
pub trait IntoBody {
    /// 把 body 铺到 `RequestBuilder` 上，content-type 一并设置
    fn apply(self, req: RequestBuilder) -> RequestBuilder;

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

/// 对应 `.body()`。`Body` 可以从 `String` / `Vec<u8>` / `Bytes` / `&'static str` /
/// `&'static [u8]` / `tokio::fs::File` 转来，也可以用 `Body::wrap_stream` 包一个流式来源。
/// content-type 按 [`ContentType`] 一并带上，不开放任意取值
#[derive(Debug)]
pub struct Raw(pub ContentType, pub Body);

impl Raw {
    /// `Raw(content_type, body.into())` 的简写
    pub fn new(content_type: ContentType, body: impl Into<Body>) -> Self {
        Self(content_type, body.into())
    }
}

impl IntoBody for () {
    fn apply(self, req: RequestBuilder) -> RequestBuilder {
        req
    }

    fn preview(&self) -> Result<Option<String>, HyErr> {
        Ok(None)
    }
}

impl<T: serde::Serialize> IntoBody for Json<T> {
    fn apply(self, req: RequestBuilder) -> RequestBuilder {
        req.json(&self.0)
    }

    fn preview(&self) -> Result<Option<String>, HyErr> {
        redact::to_redacted_json(&self.0).map(Some)
    }
}

impl<T: serde::Serialize> IntoBody for Form<T> {
    fn apply(self, req: RequestBuilder) -> RequestBuilder {
        req.form(&self.0)
    }

    fn preview(&self) -> Result<Option<String>, HyErr> {
        redact::to_redacted_json(&self.0).map(Some)
    }
}

impl IntoBody for Raw {
    fn apply(self, req: RequestBuilder) -> RequestBuilder {
        req.header(CONTENT_TYPE, self.0.as_str()).body(self.1)
    }

    /// 流式来源（`Body::wrap_stream`、`File`）读一次就消费掉，拿不到内容；二进制
    /// （protobuf、octet-stream）打出来是乱码，所以这两种只给一个标记而不是内容。
    /// 文本里的控制字符转义后再打，和响应日志一样，避免一条日志被拆成多行
    fn preview(&self) -> Result<Option<String>, HyErr> {
        let Self(content_type, body) = self;
        Ok(Some(match body.as_bytes() {
            // 是不是文本和响应日志用同一套规则；二进制（protobuf、octet-stream）只报字节数
            Some(bytes) if is_text_content_type(content_type.as_str()) => {
                to_one_line(decode_charset(content_type.as_str(), bytes)).into_owned()
            }
            Some(bytes) => format!("<{} bytes {}>", bytes.len(), content_type.as_str()),
            None => "<stream>".to_string(),
        }))
    }
}

/// boundary 和各段头由 reqwest 生成
impl IntoBody for multipart::Form {
    fn apply(self, req: RequestBuilder) -> RequestBuilder {
        req.multipart(self)
    }

    fn preview(&self) -> Result<Option<String>, HyErr> {
        Ok(Some("<multipart>".to_string()))
    }
}

// ============================ 响应体 ============================

/// 还没读的响应体，交给 [`FromBody::from_body`]：读完用 [`RespBody::bytes`]，流式接过去用
/// [`RespBody::into_stream`]。读的过程中失败报 [`RequestFailed`](super::BaseHttpErr::RequestFailed)，并打失败日志
pub struct RespBody<'a> {
    resp: reqwest::Response,
    ctx: &'a SendCtx,
}

impl<'a> RespBody<'a> {
    pub(super) fn new(resp: reqwest::Response, ctx: &'a SendCtx) -> Self {
        Self { resp, ctx }
    }

    pub fn status(&self) -> StatusCode {
        self.resp.status()
    }

    pub fn headers(&self) -> &HeaderMap {
        self.resp.headers()
    }

    /// 读完整个 body（已按透明压缩解压）。响应头到了但 body 没读完，照样算没拿到完整响应
    pub async fn bytes(self) -> Result<Bytes, HyErr> {
        let status = self.resp.status();
        let ctx = self.ctx;
        self.resp
            .bytes()
            .await
            .map_err(|e| ctx.transport_failed(e, Some(status)))
    }

    /// 不读 body，转成流交出去。之后每一块出错时报 `RequestFailed`，但不打日志：流已经交给调用方了
    pub fn into_stream(self) -> BodyStream {
        let (method, url) = (self.ctx.method.clone(), self.ctx.url.clone());
        let stream = self
            .resp
            .bytes_stream()
            .map(move |chunk| chunk.map_err(|e| request_failed(&method, url.as_str(), e)));
        BodyStream(Box::pin(stream))
    }
}

/// 响应 body 解码成什么类型，给 [`RequestConfig::send`](super::RequestConfig::send) 用。
///
/// 现成的实现：[`Bytes`]（原样）、`String`（按 `Content-Type` 的 charset 解码）、[`Json<T>`]（反序列化）、
/// `()`（丢弃 body）、[`BodyStream`]（不读，流式交给调用方，用于下载大文件）。
///
/// 一般只实现 [`FromBody::from_bytes`]：默认的 [`FromBody::from_body`] 先读完整个 body 再交给它，
/// 解码失败时还会打一条带原文的失败日志。要流式处理才重写 `from_body`
pub trait FromBody: Sized {
    /// 从还没读的响应体得到 `Self`。默认读完整个 body 交给 [`FromBody::from_bytes`]
    fn from_body(body: RespBody<'_>) -> impl Future<Output = Result<Self, HyErr>> + Send {
        async move {
            let (status, headers, ctx) = (body.status(), body.headers().clone(), body.ctx);
            let bytes = body.bytes().await?;
            Self::from_bytes(&headers, bytes.clone())
                .inspect_err(|e| ctx.decode_failed(status, &headers, &bytes, e))
        }
    }

    /// 从读完的 body 解码，失败时返回 `Err`，`send` 会原样传给调用方。`headers` 是响应头
    fn from_bytes(headers: &HeaderMap, body: Bytes) -> Result<Self, HyErr>;

    /// 解码成功后日志里怎么打这个响应。默认 `None`，成功日志里不输出 `resp`：
    /// 没有类型信息就没法打码，body 也可能很大。
    /// `Json<T>` 按 [`redact::to_redacted_json`] 重新序列化，`T` 里标了 `#[redact(..)]` 的字段会打码
    fn decoded_preview(&self) -> Option<String> {
        None
    }
}

impl FromBody for Bytes {
    fn from_bytes(_: &HeaderMap, body: Bytes) -> Result<Self, HyErr> {
        Ok(body)
    }
}

impl FromBody for String {
    /// 按 `Content-Type` 里的 charset 解码（没写或不认识就按 UTF-8），和日志、错误里的规则一致
    fn from_bytes(headers: &HeaderMap, body: Bytes) -> Result<Self, HyErr> {
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        Ok(decode_charset(content_type, &body).into_owned())
    }

    /// 调用方要的就是文本，原样打（没法打码；有敏感内容就用 `Json<T>` 标 `#[redact]`，
    /// 或者 `enable_logging(false)`）。`Bytes` 可能是二进制、`()` 不关心内容，这两个不打
    fn decoded_preview(&self) -> Option<String> {
        Some(self.clone())
    }
}

impl FromBody for () {
    fn from_bytes(_: &HeaderMap, _: Bytes) -> Result<Self, HyErr> {
        Ok(())
    }
}

/// 解码失败时 body 原文放在 `err_args["body"]`，不进对外消息（和非 2xx 一致）；
/// 响应体和预期结构对不上时，光看 serde 的报错很难定位，排查时看它和失败日志。
///
/// `T` 要同时实现 `Serialize`，日志打的是解码后重新序列化的结果：没在 `T` 里定义的字段不会出现，
/// 数值格式、字段顺序也可能和原文不同
impl<T: serde::de::DeserializeOwned + serde::Serialize> FromBody for Json<T> {
    fn from_bytes(_: &HeaderMap, body: Bytes) -> Result<Self, HyErr> {
        serde_json::from_slice(&body).map(Json).wrap_err(|| {
            err!(BaseErr::JsonError, {
                "cause": "deserialize response body failed",
                "body": String::from_utf8_lossy(&body),
            })
        })
    }

    /// 重新序列化失败时也不退回原文，否则打码就白做了
    fn decoded_preview(&self) -> Option<String> {
        Some(
            redact::to_redacted_json(&self.0)
                .unwrap_or_else(|e| format!("<log preview failed: {e}>")),
        )
    }
}

/// 流式的响应体：`send` 拿到 2xx 的响应头就返回，body 一块块自己读，用于下载大文件，不整个读进内存。
///
/// ```ignore
/// let r: HttpResponse<BodyStream> = cfg.send(&client).await?;
/// let size = r.body.write_to(tokio::fs::File::create("a.zip").await?).await?;
///
/// // 或者自己一块块处理
/// let mut r: HttpResponse<BodyStream> = cfg.send(&client).await?;
/// while let Some(chunk) = r.body.next().await {
///     handle(chunk?);
/// }
/// ```
///
/// 成功日志只在拿到响应头时打一条（没有 `resp`），之后读的过程库不再打日志；
/// 中途出错时每一块报 `RequestFailed`，由调用方处理
pub struct BodyStream(Pin<Box<dyn Stream<Item = Result<Bytes, HyErr>> + Send>>);

impl BodyStream {
    /// 读下一块，读完返回 `None`
    pub async fn next(&mut self) -> Option<Result<Bytes, HyErr>> {
        StreamExt::next(&mut self.0).await
    }

    /// 把剩下的 body 一块块写进 `writer`（文件、`Vec<u8>`、TCP 连接……任何 tokio `AsyncWrite`），
    /// 写完 flush，返回写入的总字节数。
    ///
    /// 读失败报 `RequestFailed`；写失败报 [`WriteFailed`](super::BaseHttpErr::WriteFailed)，io 错误在 source 上。
    /// 中途失败时 writer 里已经写了一部分，要不要删文件由调用方决定
    pub async fn write_to<W: AsyncWrite + Unpin>(mut self, mut writer: W) -> Result<u64, HyErr> {
        let mut written = 0u64;
        while let Some(chunk) = self.next().await {
            let chunk = chunk?;
            writer.write_all(&chunk).await.map_err(write_failed)?;
            written += chunk.len() as u64;
        }
        writer.flush().await.map_err(write_failed)?;
        Ok(written)
    }
}

impl Stream for BodyStream {
    type Item = Result<Bytes, HyErr>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.as_mut().poll_next(cx)
    }
}

impl fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BodyStream")
    }
}

impl FromBody for BodyStream {
    /// 不读 body，直接把流接过去
    fn from_body(body: RespBody<'_>) -> impl Future<Output = Result<Self, HyErr>> + Send {
        async move { Ok(body.into_stream()) }
    }

    /// 已经读完的 body 当成只有一块的流
    fn from_bytes(_: &HeaderMap, body: Bytes) -> Result<Self, HyErr> {
        Ok(BodyStream(Box::pin(stream::once(async move { Ok(body) }))))
    }
}

#[cfg(test)]
mod tests {
    use hygiea_test_support::Unserializable;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};

    use hygiea_test_support::headers::header_map;

    use crate::net::http::{BaseHttpErr, Client};
    use crate::redact::redact;

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
            let preview = Raw(ContentType::OctetStream, body).preview().unwrap();
            assert_eq!(preview.as_deref(), Some("<stream>"));
        }

        /// multipart 不打内容（可能是文件）
        #[test]
        fn multipart_shows_marker() {
            let preview = multipart::Form::new().text("k", "v").preview().unwrap();
            assert_eq!(preview.as_deref(), Some("<multipart>"));
        }
    }

    #[redact]
    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Token {
        user: String,
        #[redact(mask)]
        token: String,
    }

    /// 各个 `FromBody` 实现的解码
    mod from_body {
        use super::*;

        /// `String` 按 Content-Type 里的 charset 解码（"你好" 的 GBK 字节），没写就按 UTF-8
        #[test]
        fn string_honors_charset() {
            let gbk = Bytes::from_static(&[0xc4, 0xe3, 0xba, 0xc3]);
            let mut headers = HeaderMap::new();
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=gbk"),
            );
            assert_eq!(String::from_bytes(&headers, gbk).unwrap(), "你好");
            let utf8 = Bytes::from_static("你好".as_bytes());
            assert_eq!(String::from_bytes(&HeaderMap::new(), utf8).unwrap(), "你好");
        }

        /// `Bytes` 原样返回
        #[test]
        fn bytes_is_identity() {
            let body = Bytes::from_static(b"\x00\xff");
            assert_eq!(
                Bytes::from_bytes(&HeaderMap::new(), body.clone()).unwrap(),
                body
            );
        }

        /// `String` 按 UTF-8 解码，非法字节替换成 `�`，不会失败
        #[test]
        fn string_is_lossy() {
            let s = String::from_bytes(&HeaderMap::new(), Bytes::from_static(b"a\xffb")).unwrap();
            assert_eq!(s, "a\u{fffd}b");
        }

        /// `()` 丢弃 body，什么内容都接受
        #[test]
        fn unit_discards_body() {
            <()>::from_bytes(&HeaderMap::new(), Bytes::from_static(b"not json")).unwrap();
        }

        /// `Json<T>` 反序列化结构体
        #[test]
        fn json_decodes_struct() {
            let Json(t) = Json::<Token>::from_bytes(
                &HeaderMap::new(),
                Bytes::from_static(br#"{"user":"a","token":"t"}"#),
            )
            .unwrap();
            assert_eq!(
                t,
                Token {
                    user: "a".into(),
                    token: "t".into()
                }
            );
        }

        /// `Json<T>` 也能直接解码成标量
        #[test]
        fn json_decodes_scalars() {
            let Json(n) =
                Json::<u64>::from_bytes(&HeaderMap::new(), Bytes::from_static(b"42")).unwrap();
            assert_eq!(n, 42);
            let Json(s) =
                Json::<String>::from_bytes(&HeaderMap::new(), Bytes::from_static(br#""ok""#))
                    .unwrap();
            assert_eq!(s, "ok");
        }

        /// 结构对不上时报 JsonError：body 原文在 err_args 里方便对照，不进对外消息
        #[test]
        fn json_error_carries_raw_body() {
            let err =
                Json::<Token>::from_bytes(&HeaderMap::new(), Bytes::from_static(br#"{"user":1}"#))
                    .unwrap_err();
            assert!(err.is(BaseErr::JsonError));
            assert_eq!(err.err_args["body"], r#"{"user":1}"#);
            assert_eq!(
                err.to_string(),
                "JSON error: deserialize response body failed"
            );
        }
    }

    /// `BodyStream::write_to()`：写进调用方给的 writer
    mod write_to {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        use super::*;

        /// 写 Vec 这类内存 writer：内容和字节数都对
        #[tokio::test]
        async fn writes_all_chunks() {
            let stream =
                BodyStream::from_bytes(&HeaderMap::new(), Bytes::from_static(b"hello")).unwrap();
            let mut out = Vec::new();
            assert_eq!(stream.write_to(&mut out).await.unwrap(), 5);
            assert_eq!(out, b"hello");
        }

        /// 总是写失败的 writer
        struct Broken;

        impl AsyncWrite for Broken {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                Poll::Ready(Err(std::io::Error::other("disk full")))
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        /// 写失败报 WriteFailed（不是 RequestFailed），io 错误在 source 上
        #[tokio::test]
        async fn write_error_is_write_failed() {
            let stream =
                BodyStream::from_bytes(&HeaderMap::new(), Bytes::from_static(b"x")).unwrap();
            let err = stream.write_to(Broken).await.unwrap_err();
            assert!(err.is(BaseHttpErr::WriteFailed));
            assert!(format!("{err:#}").contains("disk full"), "{err:#}");
        }
    }

    /// `decoded_preview()`：解码成功后日志里打什么
    mod decoded_preview {
        use super::*;

        /// `Bytes`（可能是二进制）和 `()`（不关心内容）返回 `None`，成功日志里不输出 resp；
        /// `String` 调用方要的就是文本，原样返回
        #[test]
        fn raw_types() {
            assert_eq!(Bytes::from_static(b"x").decoded_preview(), None);
            assert_eq!(().decoded_preview(), None);
            assert_eq!(String::from("x").decoded_preview().as_deref(), Some("x"));
        }

        /// `Json<T>` 重新序列化，`T` 里标了的字段打码
        #[test]
        fn json_reserializes_with_masking() {
            let t = Json(Token {
                user: "a".into(),
                token: "secret".into(),
            });
            let preview = t.decoded_preview().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&preview).unwrap(),
                json!({"user":"a","token":"***"})
            );
        }

        /// 能解码、但重新序列化必定失败的类型
        #[derive(Deserialize)]
        struct DecodeOnly;

        impl Serialize for DecodeOnly {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                Unserializable.serialize(s)
            }
        }

        /// 重新序列化失败时给一个标记，不退回原文，否则打码就白做了
        #[test]
        fn json_reserialize_failure_is_marked() {
            let preview = Json(DecodeOnly).decoded_preview().unwrap();
            assert!(preview.starts_with("<log preview failed:"), "{preview}");
        }
    }

    /// 保留请求头：有专门入口或由 hyper 管理的，不允许手设
    mod reserved_headers {
        use super::*;

        /// 五个保留字段逐个拒绝，报 BaseHttpErr::InvalidHeader，大小写不影响
        #[test]
        fn every_reserved_header_is_rejected() {
            for name in [
                "authorization",
                "Content-Type",
                "content-length",
                "Transfer-Encoding",
                "connection",
            ] {
                let err = checked_headers(header_map(&[(name, "x")])).unwrap_err();
                assert!(err.is(BaseHttpErr::InvalidHeader), "{name}");
            }
        }

        /// 混在普通头里也会被拦下，整个调用失败
        #[test]
        fn one_reserved_header_fails_the_whole_map() {
            let headers = header_map(&[("x-a", "1"), ("content-type", "text/plain")]);
            assert!(checked_headers(headers).is_err());
        }

        /// 看着像但不是保留字段的，照常放行
        #[test]
        fn similar_names_are_allowed() {
            let headers = header_map(&[("x-authorization", "1"), ("content-encoding", "gzip")]);
            assert_eq!(checked_headers(headers).unwrap().len(), 2);
        }
    }

    /// `Auth` 的 Debug
    mod auth {
        use super::*;

        /// Debug 输出里凭据都是 `***`，用户名、scheme 这类非机密信息保留
        #[test]
        fn debug_masks_credentials() {
            let cases = [
                Auth::Basic {
                    username: "alice".into(),
                    password: Some("pw-secret".into()),
                },
                Auth::Bearer("tok-secret".into()),
                Auth::Custom {
                    scheme: "HMAC".into(),
                    credentials: "sig-secret".into(),
                },
            ];
            for auth in cases {
                let debug = format!("{auth:?}");
                assert!(!debug.contains("secret"), "{debug}");
                assert!(debug.contains("***"), "{debug}");
            }
            let debug = format!(
                "{:?}",
                Auth::Basic {
                    username: "alice".into(),
                    password: None
                }
            );
            assert_eq!(debug, r#"Basic { username: "alice", password: None }"#);
        }
    }
}
