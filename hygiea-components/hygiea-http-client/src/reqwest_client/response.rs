//! 响应体：读完再解码的 [`FromBytes`]（`Bytes` / `String` / `Json` / `()`，可以重试），
//! `send` 用的总入口 [`FromBody`]（`FromBytes` 自动实现，另有流式的 [`BodyStream`]）。读 body 的 [`RespBody`] 在 send.rs。
//!
//! 两个 trait 实现在「解码器」上，解码结果是关联类型 `Output`：`send::<Json<User>>` 里的 `Json<User>`
//! 只用来选解码方式，`HttpResponse.body` 是 `User`。`Bytes` / `String` / `()` / `BodyStream` 的
//! `Output` 是它们自己

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::{Stream, StreamExt};
use reqwest::header::CONTENT_TYPE;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use hygiea_core::{BaseErr, Result, ResultExt, err, redact};

use super::body::Json;
use super::error::write_failed;
use super::send::RespBody;
use super::text::decode_charset;
use super::{Bytes, HeaderMap};

/// 读完整个 body 再解码的解码器，解码结果是 [`Output`](FromBytes::Output)。现成的实现：
/// [`Bytes`]（原样）、`String`（按 `Content-Type` 的 charset 解码）、[`Json<T>`]（反序列化成 `T`）、
/// `()`（丢弃 body）。自定义解码（拆业务外壳、解密、验签……）也实现这个，`Output` 可以是拆出来的业务数据。
///
/// 实现了它的类型自动实现 [`FromBody`]，`send` 直接能用；带重试发送（见 [`RetryCtx`](super::RetryCtx)）
/// 要求解码器实现它：body 整个在内存里，失败时原始响应才留得住，交给重试判断。
///
/// `from_bytes` 返回 `Err` 就算这次没拿到想要的结果：`send` 把它包成
/// [`DecodeFailed`](super::BaseHttpErr::DecodeFailed)（原错误在 source 上）并打一条带原文的失败日志；
/// 带重试时交给重试判断。比如 `{code, msg, data}` 外壳在 `code` 不是成功时返回 `Err`，业务失败也能触发重试
pub trait FromBytes {
    /// 解码结果，也就是 `HttpResponse.body` 的类型
    type Output;

    /// 从读完的 body 解码。`headers` 是响应头
    fn from_bytes(headers: &HeaderMap, body: Bytes) -> Result<Self::Output>;

    /// 解码成功后日志里怎么打这个响应。默认 `Ok(None)`，成功日志里不输出 `resp`：
    /// 没有类型信息就没法打码，body 也可能很大。
    /// `Json<T>` 按 [`redact::to_redacted_json`] 重新序列化，`T` 里标了 `#[redact(..)]` 的字段会打码。
    /// 返回 `Err` 时 `send` 返回这个错误（请求已经成功了，只是日志打不出来）
    fn decoded_preview(_output: &Self::Output) -> Result<Option<String>> {
        Ok(None)
    }
}

/// 响应 body 怎么变成 [`Output`](FromBody::Output)，给 [`RequestConfig::send`](super::RequestConfig::send) 用。
///
/// 一般不直接实现它：实现 [`FromBytes`] 就自动有了。要流式处理（不把 body 整个读进内存）才直接实现，
/// 比如 [`BodyStream`]；这种类型不能用于带重试发送
pub trait FromBody {
    /// 得到的结果，也就是 `HttpResponse.body` 的类型
    type Output;

    /// 从还没读的响应体得到结果
    fn from_body(body: RespBody<'_>) -> impl Future<Output = Result<Self::Output>> + Send;

    /// 成功日志里的响应摘要，`Ok(None)` 表示不输出 `resp`，`Err` 时 `send` 返回这个错误。
    /// [`FromBytes`] 类型用它的 `decoded_preview`
    fn resp_preview(_output: &Self::Output) -> Result<Option<String>> {
        Ok(None)
    }
}

impl<T: FromBytes> FromBody for T
where
    T::Output: Send,
{
    type Output = T::Output;

    async fn from_body(body: RespBody<'_>) -> Result<Self::Output> {
        body.decode::<T>().await?.map_err(|f| f.err)
    }

    fn resp_preview(output: &Self::Output) -> Result<Option<String>> {
        T::decoded_preview(output)
    }
}

impl FromBytes for Bytes {
    type Output = Bytes;

    fn from_bytes(_: &HeaderMap, body: Bytes) -> Result<Bytes> {
        Ok(body)
    }
}

impl FromBytes for String {
    type Output = String;

    /// 按 `Content-Type` 里的 charset 解码（没写或不认识就按 UTF-8），和日志、错误里的规则一致
    fn from_bytes(headers: &HeaderMap, body: Bytes) -> Result<String> {
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        Ok(decode_charset(content_type, &body).into_owned())
    }

    /// 调用方要的就是文本，原样打（没法打码；有敏感内容就用 `Json<T>` 标 `#[redact]`，
    /// 或者 `enable_logging(false)`）。`Bytes` 可能是二进制、`()` 不关心内容，这两个不打
    fn decoded_preview(output: &String) -> Result<Option<String>> {
        Ok(Some(output.clone()))
    }
}

impl FromBytes for () {
    type Output = ();

    fn from_bytes(_: &HeaderMap, _: Bytes) -> Result<()> {
        Ok(())
    }
}

/// 反序列化成 `T`，`HttpResponse.body` 是 `T`。
///
/// 反序列化失败报 `JsonError`，`send` 再包成 [`DecodeFailed`](super::BaseHttpErr::DecodeFailed)，
/// body 原文在 `DecodeFailed` 的 err_args 里；响应体和预期结构对不上时，光看 serde 的报错很难定位，排查时看它和失败日志。
///
/// `T` 要同时实现 `Serialize`，日志打的是解码后重新序列化的结果：没在 `T` 里定义的字段不会出现，
/// 数值格式、字段顺序也可能和原文不同
impl<T: serde::de::DeserializeOwned + serde::Serialize> FromBytes for Json<T> {
    type Output = T;

    fn from_bytes(_: &HeaderMap, body: Bytes) -> Result<T> {
        serde_json::from_slice(&body)
            .wrap_err(|| err!(BaseErr::JsonError, "deserialize response body failed"))
    }

    /// 重新序列化失败时返回错误，不退回原文，否则打码就白做了
    fn decoded_preview(output: &T) -> Result<Option<String>> {
        redact::to_redacted_json(output).map(Some)
    }
}

/// 流式的响应体：`send` 拿到 2xx 的响应头就返回，body 一块块自己读，用于下载大文件，不整个读进内存。
///
/// ```ignore
/// let r = cfg.send::<BodyStream>(&client).await?;
/// let size = r.body.write_to(tokio::fs::File::create("a.zip").await?).await?;
///
/// // 或者自己一块块处理
/// let mut r = cfg.send::<BodyStream>(&client).await?;
/// while let Some(chunk) = r.body.next().await {
///     handle(chunk?);
/// }
/// ```
///
/// 成功日志只在拿到响应头时打一条（没有 `resp`），之后读的过程库不再打日志；
/// 中途出错时每一块报 `RequestFailed`，由调用方处理。body 没读完，不能用于带重试发送
pub struct BodyStream(pub(super) Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>);

impl BodyStream {
    /// 读下一块，读完返回 `None`
    pub async fn next(&mut self) -> Option<Result<Bytes>> {
        StreamExt::next(&mut self.0).await
    }

    /// 把剩下的 body 一块块写进 `writer`（文件、`Vec<u8>`、TCP 连接……任何 tokio `AsyncWrite`），
    /// 写完 flush，返回写入的总字节数。
    ///
    /// 读失败报 `RequestFailed`；写失败报 [`WriteFailed`](super::BaseHttpErr::WriteFailed)，io 错误在 source 上。
    /// 中途失败时 writer 里已经写了一部分，要不要删文件由调用方决定
    pub async fn write_to<W: AsyncWrite + Unpin>(mut self, mut writer: W) -> Result<u64> {
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
    type Item = Result<Bytes>;

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
    type Output = BodyStream;

    /// 不读 body，直接把流接过去
    async fn from_body(body: RespBody<'_>) -> Result<BodyStream> {
        Ok(body.into_stream())
    }
}

#[cfg(test)]
mod tests {
    use crate::reqwest_client::BaseHttpErr;
    use crate::reqwest_client::HeaderValue;
    use hygiea_core::redact::redact;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use test_support::Unserializable;

    use super::*;

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

        /// `Json<T>` 反序列化结构体，解码结果是 `T`
        #[test]
        fn json_decodes_struct() {
            let t = Json::<Token>::from_bytes(
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
            let n = Json::<u64>::from_bytes(&HeaderMap::new(), Bytes::from_static(b"42")).unwrap();
            assert_eq!(n, 42);
            let s = Json::<String>::from_bytes(&HeaderMap::new(), Bytes::from_static(br#""ok""#))
                .unwrap();
            assert_eq!(s, "ok");
        }

        /// 结构对不上时报 JsonError，serde 的原因在 source 上。body 原文由 send 包成 DecodeFailed 时带上
        #[test]
        fn json_error_is_json_error() {
            let err =
                Json::<Token>::from_bytes(&HeaderMap::new(), Bytes::from_static(br#"{"user":1}"#))
                    .unwrap_err();
            assert!(err.is(BaseErr::JsonError));
            assert!(format!("{err:#}").contains("invalid type"), "{err:#}");
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

        /// 只有一块的流
        fn one_chunk(bytes: &'static [u8]) -> BodyStream {
            let chunk = Ok(Bytes::from_static(bytes));
            BodyStream(Box::pin(futures_util::stream::once(async move { chunk })))
        }

        /// 写 Vec 这类内存 writer：内容和字节数都对
        #[tokio::test]
        async fn writes_all_chunks() {
            let stream = one_chunk(b"hello");
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
            let stream = one_chunk(b"x");
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
            assert_eq!(
                Bytes::decoded_preview(&Bytes::from_static(b"x")).unwrap(),
                None
            );
            assert_eq!(<()>::decoded_preview(&()).unwrap(), None);
            assert_eq!(
                String::decoded_preview(&String::from("x"))
                    .unwrap()
                    .as_deref(),
                Some("x")
            );
        }

        /// `Json<T>` 重新序列化，`T` 里标了的字段打码
        #[test]
        fn json_reserializes_with_masking() {
            let t = Token {
                user: "a".into(),
                token: "secret".into(),
            };
            let preview = Json::<Token>::decoded_preview(&t).unwrap().unwrap();
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

        /// 重新序列化失败时返回 JsonError，不退回原文，否则打码就白做了
        #[test]
        fn json_reserialize_failure_is_err() {
            let err = Json::<DecodeOnly>::decoded_preview(&DecodeOnly).unwrap_err();
            assert!(err.is(BaseErr::JsonError), "{err:#}");
        }
    }
}
