//! 发送：`RequestConfig::send` 以及它背后的整条流程。
//!
//! ## 发送流程
//!
//! 对外只有一个 `send`，第二个参数的类型决定走哪条路。几个函数各管一段：
//!
//! | 函数 | 位置 | 管什么 |
//! |---|---|---|
//! | `send(sender)` | send.rs，pub | 唯一入口，原样转给 `sender.send_request(self)` |
//! | `IntoSender::send_request` | retry.rs | 按 `sender` 的类型分派：`&ReqwestClient` 发一次，`(&ReqwestClient, RetryCtx)` 带重试 |
//! | `send_once` | send.rs | 发一次，响应按 [`FromBody`] 取（可以是流），失败只要 `HyErr` |
//! | `send_once_bytes` | send.rs | 发一次，响应读完按 [`FromBytes`] 解码，失败带 [`SendFailure`]（原始响应），给重试用 |
//! | `dispatch` | send.rs | 两条路共用的前半段：发出之前逐项检查、构建、打 start 日志、发出去拿到响应头 |
//! | `read_status_failure` | send.rs | 非 2xx：读完 body，交给 `SendCtx::status_failed` |
//! | `SendCtx::*_failed` / `succeeded` | send.rs | 调 logging.rs 打失败 / 成功日志，构造错误和 [`SendFailure`] |
//! | `RespBody::read_all` / `decode` | send.rs | 读 body、按 `FromBytes` 解码，失败交给 `SendCtx` |
//!
//! ```text
//! cfg.send(sender)
//!  └─ sender.send_request(cfg)                            IntoSender，retry.rs：按 sender 的类型二选一
//!      │
//!      ├─ &ReqwestClient ────────────────────────────────────── 不重试
//!      │   └─ send_once::<Decoder: FromBody>
//!      │       ├─ dispatch ───────────────────────────────┐ 共用的前半段（见下）
//!      │       ├─ 非 2xx → read_status_failure → Err(NonSuccessStatus)
//!      │       ├─ 2xx    → Decoder::from_body(RespBody)
//!      │       │           ├─ FromBytes 类型：RespBody::decode → 失败 Err(DecodeFailed)
//!      │       │           └─ BodyStream：into_stream，不读 body
//!      │       └─ 成功   → SendCtx::succeeded（打 success 日志）
//!      │
//!      └─ (&ReqwestClient, RetryCtx) ─────────────────────────── 带重试，要求 Params / Req: Clone、Decoder: FromBytes
//!          └─ loop {
//!               kept = cfg.clone()                        发出去的那份会被消费，先留一份
//!               send_once_bytes::<Decoder: FromBytes>
//!                ├─ dispatch ─────────────────────────────┤ 外层 Err（发出之前的错误）→ 直接返回，不进 Retry
//!                ├─ 非 2xx → read_status_failure → SendFailure { Status(原始响应) }
//!                ├─ 2xx    → RespBody::decode → 失败 SendFailure { Decode(原始响应) }
//!                └─ 成功   → SendCtx::succeeded → 直接返回 Ok
//!               次数用完 → 返回 failure.err
//!               retry.retry(attempt, kept, failure)
//!                ├─ Retry(cfg) / Unknown(cfg) → cfg = 它，继续循环
//!                └─ Stop(err) → 返回 err
//!             }
//!
//! dispatch(cfg, client)                                   两条路共用
//!  ├─ 发出之前（外层 Err，不打日志）：URL → 日志预览（params / body 打码）→ params 编码 → into_request（认证头、body）→ build
//!  ├─ 打 http call start
//!  └─ client.execute
//!      ├─ 失败 → 内层 Err：SendCtx::transport_failed → SendFailure { Transport }
//!      └─ 成功 → (SendCtx, 响应头已到的 Response)
//! ```
//!
//! 为什么拆成两条路：不重试时响应可以是流（[`BodyStream`](super::BodyStream)），读不读 body 由 `Decoder` 决定；
//! 重试时失败要把原始响应交给 `Retry`，body 必须先整个读进内存，所以只接受 `FromBytes`，失败带 `SendFailure`。
//! 两条路的前半段（`dispatch`）、非 2xx 处理、日志都是同一套，行为一致。
//!
//! 日志和打码的细节（预览怎么生成、哪些字段打码）见 text.rs 开头的调用链。
//!
//! ## debug 日志（`debug-log` feature）
//!
//! [`ReqwestConfig::debug`](super::ReqwestConfig) 打开时，上面那套日志换成 debug 日志（`log_debug`）：
//! 消息后面接一段 JSON（默认一行，`ReqwestClient::set_pretty` 打开后缩进成多行），无视 `enable_logging`，
//! 不打码。每次请求两条，每条都能单独看懂：
//!
//! - `http call start`：`{method, url, request: {headers, params, body}}`
//! - 结束（success / non-2xx / decode failed / failed，五种出口之一）：
//!   `{method, url, status, elapsed_ms, request, response: {headers, body}, error}`
//!
//! url 是真正发出去的地址；请求头是请求自己的头补上 client 默认头；JSON 的 params / body 嵌套成对象，
//! 别的是字符串。传输失败没有 response；`BodyStream` 不读 body，response.body 为 null。
//! 要用的东西在 `dispatch` 里算好放进 `SendCtx::debug`，各出口先调 `debug_start` / `debug_end`，
//! 返回 false（不是 debug）才打原来的日志

use std::future::Future;
use std::time::Instant;

use futures_util::StreamExt;

use hygiea_core::{HyErr, Result, redact};

use super::body::IntoBody;
use super::error::{
    decode_failed, invalid_params, invalid_url, non_success_status, request_build_failed,
    request_failed,
};
use super::logging::{
    FailedLogLevel, Failure, ReqFields, Start, log_decode_failed, log_failed, log_resp_success,
    log_start, log_status_failed,
};
use super::request::{HttpResponse, RequestConfig};
use super::response::{BodyStream, FromBody, FromBytes};
use super::text::{body_preview, body_text, to_one_line};
use super::{Bytes, HeaderMap, Method, ReqwestClient, StatusCode, Url};

impl<Params: serde::Serialize, Req: IntoBody> RequestConfig<Params, Req> {
    /// 发请求，2xx 时按 `Decoder` 把 body 变成 `Decoder::Output`（见 [`FromBody`]），否则返回
    /// `HttpClientErr::NonSuccessStatus`。要原样的字节就用 [`Bytes`](super::Bytes)（整个读进内存）；
    /// 大文件用 [`BodyStream`](super::BodyStream) 流式读，或者 `write_to` 直接写进文件。
    /// 解码器写在 turbofish 里（同一个 `Output` 可能来自不同解码器，没法从接收处的类型推断）：
    ///
    /// ```ignore
    /// let r = cfg.send::<Json<User>>(&client).await?;
    /// let user: User = r.body;
    ///
    /// // 带重试：第二个参数换成 (&client, RetryCtx)，见 RetryCtx
    /// let r = cfg.send::<Json<User>>((&client, RetryCtx::new(3, policy))).await?;
    /// ```
    ///
    /// 请求和响应不对称：`Req` 是请求体的值，`Json(x)` 本身就表明按 JSON 编码，不需要单独的编码器；
    /// `Decoder` 只给类型、不构造实例，调用的都是它的关联函数，解码结果是 `Decoder::Output`
    ///
    /// 失败时按阶段返回不同的错误：
    /// - 发出之前按 URL → 日志预览 → params → 认证头 → body 的顺序逐项检查，报第一个有问题的：
    ///   [`InvalidUrl`](super::HttpClientErr::InvalidUrl)、`JsonError`（预览序列化不了）、
    ///   [`InvalidParams`](super::HttpClientErr::InvalidParams)、[`InvalidHeader`](super::HttpClientErr::InvalidHeader)、
    ///   最后构建失败是 [`RequestBuildFailed`](super::HttpClientErr::RequestBuildFailed)。请求不发出去，也不打失败日志；
    ///   带重试时也直接返回，不交给重试判断
    /// - 发出之后的传输失败（超时、连不上、TLS 握手失败、读 body 中断）是
    ///   [`RequestFailed`](super::HttpClientErr::RequestFailed)，并打一条失败日志
    /// - 非 2xx 是 [`NonSuccessStatus`](super::HttpClientErr::NonSuccessStatus)；2xx 但 [`FromBytes::from_bytes`](super::FromBytes::from_bytes)
    ///   失败是 [`DecodeFailed`](super::HttpClientErr::DecodeFailed)。状态码和 body 原文在 `err_args` 里
    ///   （打日志可见，不渲染进对外消息）
    ///
    /// 响应日志：非 2xx 和解码失败打原文，排查问题要看对方到底回了什么；
    /// 解码成功后按 [`FromBytes::decoded_preview`](super::FromBytes::decoded_preview) 打，`Json<T>` 走 [`redact::to_redacted_json`]，可以打码
    pub async fn send<Decoder>(
        self,
        sender: impl IntoSender<Params, Req, Decoder>,
    ) -> Result<HttpResponse<Decoder::Output>>
    where
        Decoder: FromBody,
    {
        sender.send_request(self).await
    }

    /// 发一次，不重试
    pub(super) async fn send_once<Decoder: FromBody>(
        self,
        client: &ReqwestClient,
    ) -> Result<HttpResponse<Decoder::Output>> {
        let (ctx, resp) = match self.dispatch(client).await? {
            Ok(sent) => sent,
            Err(failure) => return Err(failure.err),
        };
        let (status, headers) = (resp.status(), resp.headers().clone());
        if !status.is_success() {
            return Err(read_status_failure(&ctx, resp).await?.err);
        }
        // 读 body 失败、解码失败的日志由 RespBody 打
        let body = Decoder::from_body(RespBody::new(resp, &ctx)).await?;
        ctx.succeeded(status, headers, body, Decoder::resp_preview)
    }

    /// 发一次，body 读完再解码；发出之后的失败带上重试判断要用的信息。重试时用。
    /// 外层 `Err` 是发出之前的错误或者打日志失败，重试也没用，直接返回
    pub(super) async fn send_once_bytes<Decoder: FromBytes>(
        self,
        client: &ReqwestClient,
    ) -> Result<Result<HttpResponse<Decoder::Output>, SendFailure>> {
        let (ctx, resp) = match self.dispatch(client).await? {
            Ok(sent) => sent,
            Err(failure) => return Ok(Err(failure)),
        };
        let (status, headers) = (resp.status(), resp.headers().clone());
        if !status.is_success() {
            return Ok(Err(read_status_failure(&ctx, resp).await?));
        }
        match RespBody::new(resp, &ctx).decode::<Decoder>().await? {
            Ok(body) => ctx
                .succeeded(status, headers, body, Decoder::decoded_preview)
                .map(Ok),
            Err(failure) => Ok(Err(failure)),
        }
    }

    /// 发出之前逐项检查、构建请求，然后发出去拿到响应头。
    /// 外层 `Err` 是发出之前的错误（调用方参数的问题，不打失败日志）或者打日志失败；内层 `Err` 是发送失败
    async fn dispatch(
        self,
        client: &ReqwestClient,
    ) -> Result<Result<(SendCtx, reqwest::Response), SendFailure>> {
        // into_request 会消费 self，日志要用的东西先取出来
        let (enable_logging, failed_log_level) = (self.enable_logging, self.failed_log_level);
        let method = self.method.clone();

        // 发出之前逐项检查，报第一个有问题的部分。这些都是调用方参数的问题，直接把错误还给调用方，
        // 不打失败日志。URL 最先查：它不对，后面几项都无从谈起。
        //
        // URL 要解析得了、是 http / https、带 host。reqwest 构建请求时只查 host，scheme 不对要到发送时
        // 才报错（那时会被当成 RequestFailed），这里提前拦下，归到「没发出去」的 InvalidUrl
        let parsed = Url::parse(&self.url).map_err(|e| invalid_url(&self.url).with_source(e))?;
        if !(matches!(parsed.scheme(), "http" | "https") && parsed.has_host()) {
            return Err(invalid_url(&self.url));
        }

        // 日志里代表这次请求"发了什么"的那一段：params 和 body 都算进来。params 按
        // redact::to_redacted_json 序列化成 JSON，保留原始结构和数值类型，不参与 query 编码；
        // 没设 params 时是 ()，序列化成 null，不打
        let req_preview = {
            let mut parts = Vec::new();
            let params = redact::to_redacted_json(&self.params)?;
            if params != "null" {
                parts.push(format!("params:{params}"));
            }
            if let Some(body) = self.body.preview()? {
                parts.push(format!("body:{body}"));
            }
            format!("[{}]", parts.join(", "))
        };
        // debug 日志里的 params / body 原文，into_request 会消费 self，先算好
        #[cfg(feature = "debug-log")]
        let debug_draft = client
            .debug
            .mode()
            .map(|pretty| super::debug::DebugDraft::new(pretty, &self.params, &self.body))
            .transpose()?;

        // 日志、错误和 HttpResponse.url 用的地址：和真实请求一样由 reqwest 拼 query，只是在 redact 的
        // 日志模式下拼，所以编码方式、字段顺序都和真实请求一致，params 里标了的字段打码或不出现。
        // URL 已经验证过，这里拼不出来就只可能是 params 编码不了，报 InvalidParams
        // （错误里是调用方写的 url 字符串，不含 params）
        let url = redact::scope(|| client.inner.get(&self.url).query(&self.params).build())
            .map(|req| req.url().clone())
            .map_err(|e| invalid_params(&self.method, &self.url, e))?;

        // reqwest 的 send() 本身就是 build() + execute()，拆开是为了在发出之前拿到最终的 Request。
        // 认证头、body 在 into_request 里验证（InvalidHeader）；URL、params 上面已经验证过。
        // build 再失败，能确定的只有「没构建出来」，报 RequestBuildFailed，具体原因在 source 里
        // （最常见的是 Form body 没法 urlencoded 编码）
        let request = self
            .into_request(&client.inner)?
            .build()
            .map_err(|e| request_build_failed(&method, url.as_str(), e))?;

        // 发出之后的失败（发送、读 body、解码）都要打日志，要用的东西放进 ctx，交给 RespBody。
        // sent_at 是请求发出的时刻：从这里算到 body 交给调用方，就是网络上实际花掉的时间
        let ctx = SendCtx {
            method,
            url,
            req_preview,
            enable_logging,
            failed_log_level,
            #[cfg(feature = "debug-log")]
            debug: debug_draft.map(|draft| draft.finish(&request, &client.debug)),
            sent_at: Instant::now(),
        };
        if !ctx.debug_start()? && enable_logging {
            log_start(&Start {
                method: &ctx.method,
                req: ctx.req_fields(),
            });
        }
        Ok(match client.inner.execute(request).await {
            Ok(resp) => Ok((ctx, resp)),
            Err(e) => Err(ctx.transport_failed(e, None)?),
        })
    }
}

/// 非 2xx：读完 body（一般是很小的错误页），读失败按传输失败报。外层 `Err` 是打日志失败
async fn read_status_failure(ctx: &SendCtx, resp: reqwest::Response) -> Result<SendFailure> {
    let (status, headers) = (resp.status(), resp.headers().clone());
    match RespBody::new(resp, ctx).read_all().await? {
        Ok(body) => ctx.status_failed(status, headers, body),
        Err(failure) => Ok(failure),
    }
}

/// 一次发送在请求发出去之后失败了。`err` 就是不重试时 `send` 会返回的错误
#[derive(Debug)]
pub struct SendFailure {
    pub err: HyErr,
    pub stage: FailStage,
}

/// 失败在哪一步，以及那一步拿到了什么
#[derive(Debug)]
pub enum FailStage {
    /// 没拿到完整响应：发送失败（连不上、超时、TLS 握手失败），或者读 body 时中断。
    /// 读 body 中断时 `status` 有值。`err` 是 [`RequestFailed`](super::HttpClientErr::RequestFailed)
    Transport {
        /// reqwest 报的超时（client 或本次请求的 timeout、read_timeout、connect_timeout）
        timeout: bool,
        /// 建连阶段失败（DNS、TCP、TLS）
        connect: bool,
        status: Option<StatusCode>,
    },
    /// 非 2xx，原始的 status、headers、body 字节都在这里（`Retry-After` 之类的头可以直接读）。
    /// `err` 是 [`NonSuccessStatus`](super::HttpClientErr::NonSuccessStatus)
    Status(Box<HttpResponse<Bytes>>),
    /// 2xx 但 [`FromBytes::from_bytes`] 失败，原始响应同样保留。
    /// `err` 是 [`DecodeFailed`](super::HttpClientErr::DecodeFailed)，`from_bytes` 报的错在它的 source 上
    Decode(Box<HttpResponse<Bytes>>),
}

// ---- sealed trait：只让本 crate 给 IntoSender 加实现 ----
//
// 这是 Rust 里常见的惯用写法（标准库和很多库都这么做）：`IntoSender` 要求先实现 `Sealed`，
// 而 `Sealed` 所在的 `sealed` 模块是私有的，外部 crate 写不出 `sealed::Sealed` 这个路径，
// 也就没法给自己的类型实现 `Sealed`，自然实现不了 `IntoSender`。
//
// 能看不能实现：`Sealed` 本身标 `pub`，因为它出现在公开 trait `IntoSender` 的约束里，约束里的 trait
// 比 `IntoSender` 更私有会触发 `private_bounds` 警告（Rust 1.74 之前是编译错误）；而模块私有又保证外面拿不到它的名字。
// 两者组合起来，外部能用 `IntoSender`（调用 send 时传 `&client` 或 `(&client, RetryCtx)`），
// 但不能扩展它。
//
// 为什么要封住：`IntoSender` 只是让 `send` 的第二个参数能接两种类型，`send_request` 是内部实现细节
// （还标了 `#[doc(hidden)]`）。封住之后，以后改 `send_request` 的签名、再加一种参数类型，都不算破坏性变更，
// 因为保证了外面没有别的实现
//
// 注意 sealed 只挡「实现」不挡「调用」：trait 方法的可见性跟着 trait 走，`IntoSender` 是 pub，
// 外部写 `(&client).send_request(cfg)` 也能编译，效果和 `cfg.send(&client)` 一样，只是文档里看不到。
//
// ---- 为什么要有 IntoSender 这个 trait ----
//
// 需求是一个 `send` 同时接 `&client` 和 `(&client, RetryCtx)`。Rust 没有函数重载（同名函数不能按参数类型
// 写两份），想让一个参数接多种类型，标准做法就是定义一个 trait，让每种类型各自实现它，函数参数写成
// `impl 这个Trait`。标准库的 `Into<T>`、`AsRef<Path>`，tokio 的 `ToSocketAddrs`、reqwest 的 `IntoUrl`
// 都是这个套路。命名跟本 crate 的 `IntoBody`、`IntoHeaders` 一致：`IntoSender` 表示「能当发送方的东西」。
// 方法叫 `send_request` 而不是 `into_sender`：它不做类型转换，直接把请求发出去
//
// 用 trait 还有一个好处：每个实现可以有自己的约束。`&ReqwestClient` 的实现只要求 `Decoder: FromBody`；
// `(&ReqwestClient, RetryCtx)` 的实现额外要求 `Params: Clone`、`Req: Clone`、`Decoder: FromBytes`。
// 所以不重试时流式 body / `BodyStream` 照常能用，开重试时才要求能重发，编译期检查。
//
// 考虑过的其他写法：
// - 分成 `send` 和 `send_with_retry` 两个方法：最直接，但两个入口；按讨论的结论要一个入口
// - 第二个参数用 enum（`Sender::Once(&client)` / `Sender::Retry(&client, ctx)`）：enum 的各个变体共用同一组
//   泛型约束，没法做到「只有 Retry 变体要求 Clone」，要么全都要求 Clone（流式 body 就不能用了），
//   要么把检查推迟到运行时
pub(super) mod sealed {
    pub trait Sealed {}
}

impl sealed::Sealed for &ReqwestClient {}

/// [`RequestConfig::send`] 的第二个参数：`&ReqwestClient`（发一次）或 `(&ReqwestClient, RetryCtx<T>)`（带重试）。
/// `Decoder` 是解码器，见 [`FromBody`]。不需要自己实现。两个实现分别调 `send_once` / `send_once_bytes`，
/// 整体流程见本文件开头
pub trait IntoSender<Params, Req, Decoder: FromBody>: sealed::Sealed {
    #[doc(hidden)]
    fn send_request(
        self,
        cfg: RequestConfig<Params, Req>,
    ) -> impl Future<Output = Result<HttpResponse<Decoder::Output>>> + Send;
}

impl<Params, Req, Decoder> IntoSender<Params, Req, Decoder> for &ReqwestClient
where
    Params: serde::Serialize + Send,
    Req: IntoBody + Send,
    Decoder: FromBody,
    Decoder::Output: Send,
{
    fn send_request(
        self,
        cfg: RequestConfig<Params, Req>,
    ) -> impl Future<Output = Result<HttpResponse<Decoder::Output>>> + Send {
        cfg.send_once::<Decoder>(self)
    }
}

// ============================ 发出之后的上下文 ============================

/// 请求发出之后，读 body 期间要用的上下文：发送 / 读 body 失败、非 2xx、解码失败时打日志并构造
/// [`SendFailure`]，成功时打成功日志。send 建好之后交给 [`RespBody`](super::RespBody)
pub(super) struct SendCtx {
    pub(super) method: Method,
    /// 打码后的地址
    pub(super) url: Url,
    pub(super) req_preview: String,
    pub(super) enable_logging: bool,
    pub(super) failed_log_level: FailedLogLevel,
    /// debug 日志要用的原文，debug 没开时是 `None`
    #[cfg(feature = "debug-log")]
    debug: Option<super::debug::DebugCtx>,
    /// 请求发出的时刻，从这里算耗时
    pub(super) sent_at: Instant,
}

impl SendCtx {
    /// 平时日志里请求那一侧的字段：打码后的地址和请求摘要
    fn req_fields(&self) -> ReqFields<'_> {
        ReqFields {
            url: self.url.as_str(),
            req: &self.req_preview,
        }
    }

    // ---- debug 日志：cfg 只出现在这几个方法里。返回 Ok(false) 表示不是 debug，调用方打原来的日志；
    // 序列化失败返回 Err，一路传给 send ----

    /// debug 时打 start（请求那一侧）
    fn debug_start(&self) -> Result<bool> {
        #[cfg(feature = "debug-log")]
        if let Some(debug) = &self.debug {
            debug.log_start()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// debug 时打结束那条：请求加响应。`level` 为 `None` 是成功（INFO）。
    /// `response` 为 `None` 表示没拿到响应；body 不给时用 read_all 存下的那份
    fn debug_end(
        &self,
        level: Option<FailedLogLevel>,
        message: &str,
        status: Option<StatusCode>,
        response: Option<(&HeaderMap, Option<&Bytes>)>,
        error: Option<&HyErr>,
    ) -> Result<bool> {
        #[cfg(feature = "debug-log")]
        if let Some(debug) = &self.debug {
            let elapsed_ms = self.sent_at.elapsed().as_millis();
            debug.log_end(level, message, status, response, error, elapsed_ms)?;
            return Ok(true);
        }
        #[cfg(not(feature = "debug-log"))]
        let _ = (level, message, status, response, error);
        Ok(false)
    }

    /// RespBody 读完 body 后调，debug 时存一份给成功日志
    fn keep_resp_body(&self, body: &Bytes) {
        #[cfg(feature = "debug-log")]
        if let Some(debug) = &self.debug {
            debug.keep_resp_body(body);
        }
        #[cfg(not(feature = "debug-log"))]
        let _ = body;
    }

    /// 拿到了响应时的 `HttpResponse`，耗时算到现在
    fn response<Resp>(
        &self,
        status: StatusCode,
        headers: HeaderMap,
        body: Resp,
    ) -> HttpResponse<Resp> {
        HttpResponse {
            method: self.method.clone(),
            status,
            headers,
            url: self.url.clone(),
            elapsed: self.sent_at.elapsed(),
            body,
        }
    }

    /// 发出之后的传输失败（发送、读 body）：包成 HttpClientErr::RequestFailed、打一条失败日志。
    /// 读 body 时已经有状态码，发送失败时没有
    pub(super) fn transport_failed(
        &self,
        e: reqwest::Error,
        status: Option<StatusCode>,
    ) -> Result<SendFailure> {
        let stage = FailStage::Transport {
            timeout: e.is_timeout(),
            connect: e.is_connect(),
            status,
        };
        let err = request_failed(&self.method, self.url.as_str(), status, e);
        let level = self.failed_log_level;
        if !self.debug_end(Some(level), "http call failed", status, None, Some(&err))? {
            let failure = Failure {
                status,
                elapsed_ms: Some(self.sent_at.elapsed().as_millis()),
                error: Some(&err),
                ..Failure::new(&self.method, self.req_fields())
            };
            log_failed(level, "http call failed", &failure);
        }
        Ok(SendFailure { err, stage })
    }

    /// 非 2xx，body 已经读完：打失败日志，报 NonSuccessStatus，原始响应留给重试判断
    pub(super) fn status_failed(
        &self,
        status: StatusCode,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<SendFailure> {
        let resp = self.response(status, headers, body);
        let level = self.failed_log_level;
        let response = Some((&resp.headers, Some(&resp.body)));
        if !self.debug_end(
            Some(level),
            "http call non-2xx",
            Some(status),
            response,
            None,
        )? {
            log_status_failed(
                level,
                &resp,
                self.req_fields(),
                &body_preview(&resp.headers, &resp.body),
            );
        }
        // 错误里放 body_text：和日志预览一样按 Content-Type / charset 解码，但不转义换行，
        // 调用方拿 err_args["body"] 去解析对方的错误格式时内容和原文一致
        let err = non_success_status(
            &resp.method,
            resp.status,
            resp.url.as_str(),
            &body_text(&resp.headers, &resp.body),
        );
        Ok(SendFailure {
            err,
            stage: FailStage::Status(Box::new(resp)),
        })
    }

    /// 2xx 但解码失败：打失败日志（带上原文方便对照），报 DecodeFailed，`e` 挂在 source 上
    pub(super) fn decode_failed(
        &self,
        status: StatusCode,
        headers: HeaderMap,
        body: Bytes,
        e: HyErr,
    ) -> Result<SendFailure> {
        let resp = self.response(status, headers, body);
        let level = self.failed_log_level;
        let response = Some((&resp.headers, Some(&resp.body)));
        let message = "http call decode failed";
        if !self.debug_end(Some(level), message, Some(status), response, Some(&e))? {
            log_decode_failed(
                level,
                &resp,
                self.req_fields(),
                &e,
                &body_preview(&resp.headers, &resp.body),
            );
        }
        let err = decode_failed(
            &resp.method,
            resp.status,
            resp.url.as_str(),
            &body_text(&resp.headers, &resp.body),
            e,
        );
        Ok(SendFailure {
            err,
            stage: FailStage::Decode(Box::new(resp)),
        })
    }

    /// 成功：debug 时打 debug 日志，否则按 enable_logging 打成功日志。`preview` 是类型提供的（打码后的）响应摘要，
    /// 关了日志时不调用，省得白算。打日志失败（预览或 debug 日志序列化不了）返回 Err：请求已经成功了，
    /// 但调用方拿到的是这个错误
    pub(super) fn succeeded<Resp>(
        &self,
        status: StatusCode,
        headers: HeaderMap,
        body: Resp,
        preview: impl FnOnce(&Resp) -> Result<Option<String>>,
    ) -> Result<HttpResponse<Resp>> {
        let resp = self.response(status, headers, body);
        let response = Some((&resp.headers, None));
        let logged = self.debug_end(None, "http call success", Some(status), response, None)?;
        if !logged && self.enable_logging {
            let resp_preview = preview(&resp.body)?.map(|p| to_one_line(p.into()).into_owned());
            log_resp_success(&resp, self.req_fields(), resp_preview.as_deref());
        }
        Ok(resp)
    }
}

/// 还没读的响应体，交给 [`FromBody::from_body`]：读完用 [`RespBody::bytes`]，流式接过去用
/// [`RespBody::into_stream`]。读的过程中失败报 [`RequestFailed`](super::HttpClientErr::RequestFailed)，并打失败日志
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
    pub async fn bytes(self) -> Result<Bytes> {
        self.read_all().await?.map_err(|f| f.err)
    }

    /// 同 [`RespBody::bytes`]，失败时带上重试判断要用的信息。外层 `Err` 是打日志失败
    pub(super) async fn read_all(self) -> Result<Result<Bytes, SendFailure>> {
        let status = self.resp.status();
        let ctx = self.ctx;
        match self.resp.bytes().await {
            Ok(body) => {
                ctx.keep_resp_body(&body);
                Ok(Ok(body))
            }
            Err(e) => ctx.transport_failed(e, Some(status)).map(Err),
        }
    }

    /// 读完 body 并按 [`FromBytes`] 解码。解码失败时打失败日志，报
    /// [`DecodeFailed`](super::HttpClientErr::DecodeFailed)，原始响应留在 [`FailStage::Decode`](super::FailStage::Decode) 里。
    /// 外层 `Err` 是打日志失败
    pub(super) async fn decode<Decoder: FromBytes>(
        self,
    ) -> Result<Result<Decoder::Output, SendFailure>> {
        let (status, headers, ctx) = (self.status(), self.headers().clone(), self.ctx);
        let bytes = match self.read_all().await? {
            Ok(bytes) => bytes,
            Err(failure) => return Ok(Err(failure)),
        };
        match Decoder::from_bytes(&headers, bytes.clone()) {
            Ok(body) => Ok(Ok(body)),
            Err(e) => ctx.decode_failed(status, headers, bytes, e).map(Err),
        }
    }

    /// 不读 body，转成流交出去。之后每一块出错时报 `RequestFailed`，但不打日志：流已经交给调用方了
    pub fn into_stream(self) -> BodyStream {
        let (method, url, status) = (
            self.ctx.method.clone(),
            self.ctx.url.clone(),
            self.resp.status(),
        );
        let stream = self.resp.bytes_stream().map(move |chunk| {
            chunk.map_err(|e| request_failed(&method, url.as_str(), Some(status), e))
        });
        BodyStream(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use crate::reqwest_client::HttpClientErr;
    use crate::reqwest_client::headers::Auth;
    use crate::reqwest_client::{Form, Json, ReqwestClient, ReqwestConfig};
    use hygiea_core::BaseErr;
    use hygiea_core::app::ConfigResource;
    use hygiea_core::redact::redact;
    use reqwest::Client;
    use serde::Serialize;
    use std::collections::BTreeMap;
    use std::error::Error as _;
    use test_support::Unserializable;
    use test_support::logs::capture;

    use super::*;

    const URL: &str = "http://127.0.0.1/login";

    #[redact]
    #[derive(Clone, Serialize)]
    struct Login {
        username: &'static str,
        #[redact(mask)]
        password: &'static str,
        #[redact(skip)]
        device_id: &'static str,
    }

    const LOGIN: Login = Login {
        username: "alice",
        password: "p@ss",
        device_id: "dev-1",
    };

    async fn client() -> ReqwestClient {
        ReqwestClient::from_config(&ReqwestConfig::default())
            .await
            .unwrap()
    }

    /// 构建出最终的 reqwest::Request，看真正要发出去的东西
    fn build<P: Serialize, B: IntoBody>(cfg: RequestConfig<P, B>) -> reqwest::Request {
        cfg.into_request(&Client::new()).unwrap().build().unwrap()
    }

    /// 请求还没发出去就失败的情况（不需要网络）：按 URL → 预览 → params → 认证头 → body 的顺序检查，
    /// 报第一个有问题的部分；都是调用方参数的问题，直接把错误还给调用方，不打任何日志
    mod send_before_network {
        use super::*;

        /// 发请求并断言：报的是 `kind`、没有打任何日志，返回错误方便继续检查
        async fn send_err<P: Serialize + Send, B: IntoBody + Send>(
            cfg: RequestConfig<P, B>,
            kind: HttpClientErr,
        ) -> HyErr {
            let (out, _guard) = capture();
            let err = cfg.send::<Bytes>(&client().await).await.unwrap_err();
            assert!(err.is(kind), "{err:#}");
            assert_eq!(out.text(), "");
            err
        }

        /// URL 非法：InvalidUrl，参数里是调用方写的 url
        #[tokio::test]
        async fn invalid_url() {
            let err = send_err(
                RequestConfig::plain(Method::GET, "not a url"),
                HttpClientErr::InvalidUrl,
            )
            .await;
            assert_eq!(err.err_args()["url"], "not a url");
            // url 的解析错误挂在 source 上
            assert!(err.source().is_some());
        }

        /// 能解析但不是 http / https，或者没有 host：同样在发出之前报 InvalidUrl，
        /// 而不是发送时的 RequestFailed（这类 reqwest 构建时不报错，要到发送时才报）
        #[tokio::test]
        async fn unsupported_scheme() {
            for url in ["ftp://127.0.0.1/f", "mailto:a@example.com", "file:///tmp/x"] {
                send_err(
                    RequestConfig::plain(Method::GET, url),
                    HttpClientErr::InvalidUrl,
                )
                .await;
            }
        }

        /// URL 的检查排在最前：URL 和 params 都有问题时，报的是 InvalidUrl
        #[tokio::test]
        async fn url_is_checked_first() {
            send_err(
                RequestConfig::with_params(Method::GET, "not a url", Unserializable),
                HttpClientErr::InvalidUrl,
            )
            .await;
        }

        /// 日志预览序列化失败：JsonError
        #[tokio::test]
        async fn preview_error_is_json_error() {
            let (out, _guard) = capture();
            let err = RequestConfig::with_params(Method::GET, URL, Unserializable)
                .send::<Bytes>(&client().await)
                .await
                .unwrap_err();
            assert!(err.is(BaseErr::JsonError));
            assert_eq!(out.text(), "");
        }

        /// params 能转成 JSON、却没法 urlencoded 编码：InvalidParams，参数里是调用方写的 url
        #[tokio::test]
        async fn unencodable_params() {
            let err = send_err(
                RequestConfig::with_params(Method::GET, URL, [("a", [1, 2])]),
                HttpClientErr::InvalidParams,
            )
            .await;
            assert_eq!(err.err_args()["url"], URL);
            // 原始的 reqwest 构建错误挂在 source 上
            let source = err.source().unwrap().downcast_ref::<reqwest::Error>();
            assert!(source.unwrap().is_builder());
        }

        /// 认证头里有非法字符：InvalidHeader，错误信息里不带凭据原文
        #[tokio::test]
        async fn invalid_auth_header() {
            let cfg = RequestConfig::plain(Method::GET, URL).auth(Auth::Custom {
                scheme: "X".into(),
                credentials: "secret\nvalue".into(),
            });
            let err = send_err(cfg, HttpClientErr::InvalidHeader).await;
            assert!(!format!("{err:#}").contains("secret"));
        }

        /// Multipart 某段的 mime 不合法：InvalidHeader，错误里指出是哪一段
        #[tokio::test]
        async fn invalid_multipart_mime() {
            use crate::reqwest_client::{Multipart, MultipartPart};

            let body =
                Multipart::new().part("avatar", MultipartPart::bytes("x").mime("not a mime"));
            let cfg = RequestConfig::with_body(Method::POST, URL, body);
            let err = send_err(cfg, HttpClientErr::InvalidHeader).await;
            assert!(format!("{err}").contains("avatar"), "{err}");
        }

        /// Form body 能转成 JSON、却没法 urlencoded 编码：RequestBuildFailed，错误里是打码后的地址
        #[tokio::test]
        async fn unencodable_body() {
            let cfg = RequestConfig::new(Method::POST, URL, &LOGIN, Form([("a", [1, 2])]));
            let err = send_err(cfg, HttpClientErr::RequestBuildFailed).await;
            assert_eq!(
                err.err_args()["url"],
                "http://127.0.0.1/login?username=alice&password=***"
            );
        }
    }

    /// 请求真的发出去的情况：发到拒绝连接的本地端口，必定是 RequestFailed，不需要起服务。
    /// 错误和失败日志里带着打码后的地址（`url`）和请求预览（`req`），用来检查这两样
    mod sent {
        use super::*;

        /// 拒绝连接的端口
        const REFUSED: &str = "http://127.0.0.1:1/login";

        /// 发请求，断言是 RequestFailed，返回错误和这期间打的日志
        async fn refused<P: Serialize + Send, B: IntoBody + Send>(
            cfg: RequestConfig<P, B>,
        ) -> (HyErr, String) {
            let (out, _guard) = capture();
            let err = cfg.send::<Bytes>(&client().await).await.unwrap_err();
            assert!(err.is(HttpClientErr::RequestFailed), "{err:#}");
            (err, out.text())
        }

        /// 带 host 的 http / https 地址都能通过 URL 检查，端口、路径、query 都不影响
        #[tokio::test]
        async fn accepts_http_and_https() {
            for url in ["http://127.0.0.1:1/x", "https://127.0.0.1:1/a?b=1"] {
                refused(RequestConfig::plain(Method::GET, url)).await;
            }
        }

        /// 地址里 params 的 mask 字段显示成 `***`，skip 的不出现；真正发出去的 query 仍是原文
        #[tokio::test]
        async fn url_masks_and_skips_params() {
            let (err, _) = refused(RequestConfig::with_params(Method::GET, REFUSED, &LOGIN)).await;
            assert_eq!(
                err.err_args()["url"],
                "http://127.0.0.1:1/login?username=alice&password=***"
            );
            let cfg = RequestConfig::with_params(Method::GET, REFUSED, &LOGIN);
            assert_eq!(
                build(cfg).url().query(),
                Some("username=alice&password=p%40ss&device_id=dev-1")
            );
        }

        /// URL 字符串里原本写的 query 原样保留（没有结构，没法打码），params 追加在后面
        #[tokio::test]
        async fn url_keeps_query_written_in_url() {
            let cfg = RequestConfig::with_params(Method::GET, "http://127.0.0.1:1/p?a=1", &LOGIN);
            let (err, _) = refused(cfg).await;
            assert_eq!(
                err.err_args()["url"],
                "http://127.0.0.1:1/p?a=1&username=alice&password=***"
            );
        }

        /// 没有 params 时就是原地址，不会多出一个 `?`
        #[tokio::test]
        async fn url_without_params_is_untouched() {
            let (err, _) = refused(RequestConfig::plain(Method::GET, REFUSED)).await;
            assert_eq!(err.err_args()["url"], REFUSED);
        }

        /// 什么都没有时 req 是空括号
        #[tokio::test]
        async fn req_empty_is_brackets() {
            let (_, logs) = refused(RequestConfig::plain(Method::GET, REFUSED)).await;
            assert!(logs.contains("req=[]"), "{logs}");
        }

        /// req 里 params 在前、body 在后，逗号分隔
        #[tokio::test]
        async fn req_params_then_body() {
            let cfg = RequestConfig::new(Method::POST, REFUSED, [("a", 1)], Json("b"));
            let (_, logs) = refused(cfg).await;
            assert!(
                logs.contains(r#"req=[params:[["a",1]], body:"b"]"#),
                "{logs}"
            );
        }

        /// req 里 params 和 Json / Form body 都按 redact 打码
        #[tokio::test]
        async fn req_params_and_body_are_masked() {
            let (_, json) = refused(RequestConfig::new(
                Method::POST,
                REFUSED,
                &LOGIN,
                Json(&LOGIN),
            ))
            .await;
            let (_, form) = refused(RequestConfig::with_body(
                Method::POST,
                REFUSED,
                Form(&LOGIN),
            ))
            .await;
            for logs in [json, form] {
                assert!(!logs.contains("p@ss"), "{logs}");
                assert!(!logs.contains("dev-1"), "{logs}");
                assert!(logs.contains(r#""password":"***""#), "{logs}");
            }
        }

        /// req 保留原始数值类型，数字不会变成字符串
        #[tokio::test]
        async fn req_keeps_value_types() {
            let params = BTreeMap::from([("n", 1.5)]);
            let (_, logs) =
                refused(RequestConfig::with_params(Method::GET, REFUSED, &params)).await;
            assert!(logs.contains(r#"req=[params:{"n":1.5}]"#), "{logs}");
        }
    }

    /// debug 打开时（发到拒绝连接的端口，看 start 和传输失败两条）：JSON，url、params、body 是原文，
    /// 带请求头，enable_logging 关了也打。成功、非 2xx 的日志和服务端收到的头对照在集成测试里
    #[cfg(feature = "debug-log")]
    mod debug {
        use serde_json::{Value, json};
        use test_support::headers::header_map;

        use super::*;

        const REFUSED: &str = "http://127.0.0.1:1/login";

        async fn debug_client() -> ReqwestClient {
            ReqwestClient::from_config(
                &ReqwestConfig {
                    user_agent: Some("ua".into()),
                    ..ReqwestConfig::default()
                }
                .default_headers(header_map(&[("x-default", "d"), ("x-both", "client")]))
                .unwrap()
                .debug(true),
            )
            .await
            .unwrap()
        }

        /// 发一次（必定传输失败），返回打出来的日志
        async fn logs(cfg: RequestConfig<&Login, Json<&Login>>, client: &ReqwestClient) -> String {
            let (out, _guard) = capture();
            cfg.send::<Bytes>(client).await.unwrap_err();
            out.text()
        }

        /// 日志里 `message` 后面的那段 JSON（紧凑时隔一个空格，pretty 时隔一个换行）
        fn debug_json(log: &str, message: &str) -> Value {
            let at = [" {", "\n{"]
                .iter()
                .find_map(|sep| log.find(&format!("{message}{sep}")))
                .unwrap_or_else(|| panic!("no `{message}` in:\n{log}"));
            serde_json::Deserializer::from_str(&log[at + message.len()..])
                .into_iter::<Value>()
                .next()
                .unwrap()
                .unwrap()
        }

        fn login() -> RequestConfig<&'static Login, Json<&'static Login>> {
            RequestConfig::new(Method::POST, REFUSED, &LOGIN, Json(&LOGIN))
                .headers(header_map(&[("x-both", "request")]))
                .unwrap()
                .enable_logging(false)
        }

        /// start 是请求那一侧；url、params、body 原文，不打码
        #[tokio::test]
        async fn start_is_request() {
            let log = logs(login(), &debug_client().await).await;
            let start = debug_json(&log, "http call start");
            let login = json!({"username": "alice", "password": "p@ss", "device_id": "dev-1"});
            assert_eq!(start["method"], "POST");
            assert_eq!(
                start["url"],
                "http://127.0.0.1:1/login?username=alice&password=p%40ss&device_id=dev-1"
            );
            assert_eq!(start["request"]["params"], login);
            assert_eq!(start["request"]["body"], login);
            assert_eq!(start.as_object().unwrap().len(), 3, "{start:#}");
        }

        /// 请求头：请求自己的头（含 Json 自动加的 content-type）补上 client 默认头，同名以请求的为准
        #[tokio::test]
        async fn req_headers_merge_client_defaults() {
            let log = logs(login(), &debug_client().await).await;
            let headers = &debug_json(&log, "http call start")["request"]["headers"];
            assert_eq!(
                *headers,
                json!({
                    "x-both": "request",
                    "content-type": "application/json",
                    "accept": "*/*",
                    "user-agent": "ua",
                    "x-default": "d",
                })
            );
        }

        /// 传输失败：按失败级别打，带完整的请求和错误，没有状态和响应。
        /// error 是返回给调用方的错误，里面的地址仍然打码
        #[tokio::test]
        async fn transport_failed_has_request_and_error() {
            let log = logs(login(), &debug_client().await).await;
            assert!(log.contains(" WARN "), "{log}");
            let start = debug_json(&log, "http call start");
            let failed = debug_json(&log, "http call failed");
            assert_eq!(failed["url"], start["url"]);
            assert_eq!(failed["request"], start["request"]);
            assert_eq!(failed["status"], Value::Null);
            assert_eq!(failed["response"], Value::Null);
            assert!(failed["elapsed_ms"].is_u64(), "{failed:#}");
            let error = failed["error"].as_str().unwrap();
            assert!(error.contains("password=***"), "{error}");
        }

        /// 默认一行；set_pretty 后 JSON 另起一行、缩进；set_debug(false) 后回到原来的日志。
        /// 开关在发出前读，改了对下一次请求生效
        #[tokio::test]
        async fn switches_take_effect_on_next_request() {
            let client = debug_client().await;
            let compact = logs(login(), &client).await;
            assert_eq!(compact.lines().count(), 2, "{compact}");
            assert!(
                compact.contains(r#"http call start {"method":"POST""#),
                "{compact}"
            );

            client.set_pretty(true);
            let pretty = logs(login(), &client).await;
            assert!(
                pretty.contains("http call start\n{\n  \"method\": \"POST\""),
                "{pretty}"
            );
            assert_eq!(
                debug_json(&pretty, "http call start"),
                debug_json(&compact, "http call start")
            );

            client.set_debug(false);
            let off = logs(login(), &client).await;
            assert_eq!(off.lines().count(), 1, "{off}");
            assert!(off.contains("http call failed method=POST"), "{off}");
        }

        /// debug 关时和原来一样：enable_logging 关了只有一行失败日志，打码，没有 JSON
        #[tokio::test]
        async fn off_keeps_original_logs() {
            let log = logs(login(), &client().await).await;
            assert_eq!(log.lines().count(), 1, "{log}");
            assert!(log.contains("http call failed method=POST"), "{log}");
            assert!(log.contains("password=***"), "{log}");
        }
    }
}
