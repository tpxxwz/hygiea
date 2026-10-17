//! HTTP 客户端的 reqwest 实现：reqwest 的一层薄封装。
//!
//! - client：[`ReqwestConfig`]（client 级配置，配置文件里的 [`HeaderMapConfig`]、[`ProxyConfig`]）造出长期持有、共享连接池的
//!   [`ReqwestClient`]；应用组件 [`ReqwestComponent`]
//! - 一次请求：[`RequestConfig`]（纯数据），请求头 [`IntoHeaders`]、认证 [`Auth`]、请求体 [`IntoBody`]，
//!   `send` 的参数 [`IntoSender`]，结果 [`HttpResponse`]
//! - 响应体：[`FromBytes`]（读完再解码，可以重试）、[`FromBody`]（总入口）、[`BodyStream`]（流式）
//! - 重试：[`RetryCtx`]、[`Retry`]、[`RetryDecision`]，失败信息 [`SendFailure`]
//! - 日志：[`FailedLogLevel`]；错误：[`HttpClientErr`]
//!
//! 各文件的职责见它们自己的文档（`tools/docs.py tree hygiea-components/hygiea-http-client/src`），
//! 发送流程、日志怎么生成、debug 日志的实现见仓库里的 `docs/design/http-client.md`。
//!
//! 请求和响应日志里的字段打码走 [`hygiea_core::redact`]。每次请求默认打 `http call start` / `http call success`
//! 两条 info 日志，失败（传输失败、非 2xx、解码失败）按 [`FailedLogLevel`] 打一条。
//!
//! # 用法：请求和日志打码
//!
//! 每次请求在 INFO 级别打 `http call start` / `http call success`，失败打 WARN（可配置）。标了 `#[redact(mask)]` 的字段在这些日志里显示为 `"***"`（包括 params、请求体、URL，以及 `send` 解码出的响应体），实际发送和接收的仍是真实值。非 2xx 和解码失败的响应原样记录，因为排查问题需要看原文。
//!
//! ```ignore
//! use hygiea::Result;
//! use hygiea::http_client::reqwest_client::{Json, Method, ReqwestClient, ReqwestConfig, RequestConfig};
//! use hygiea::redact::redact;
//! use serde::{Deserialize, Serialize};
//!
//! // #[redact] 必须写在 #[derive(Serialize)] 上面
//! #[redact]
//! #[derive(Serialize)]
//! struct LoginReq {
//!     user: String,
//!     #[redact(mask)]
//!     password: String,
//! }
//!
//! #[redact]
//! #[derive(Serialize, Deserialize)]
//! struct LoginResp {
//!     #[redact(mask)]
//!     token: String,
//! }
//!
//! async fn login(client: &ReqwestClient) -> Result<String> {
//!     let req = LoginReq { user: "alice".into(), password: "p@ss".into() };
//!     // 请求体 Json(..) 表示按 JSON 编码；send::<Json<LoginResp>> 表示按 JSON 解码，resp.body 是 LoginResp
//!     let resp = RequestConfig::with_body(Method::POST, "https://api.example.com/login", Json(req))
//!         .send::<Json<LoginResp>>(client)
//!         .await?;
//!     Ok(resp.body.token)
//! }
//!
//! // client 只建一次并共享：连接池在它里面
//! // let client = ReqwestClient::from_config(&ReqwestConfig::default(), &RegistryConfig::default()).await?; // from_config 来自 hygiea::app::ConfigResource
//! ```
//!
//! `send::<Decoder>` 的 `Decoder` 是解码器：`Json<T>` 解出 `T`，`String` / `Bytes` / `()` / `BodyStream` 解出它们自己。`resp.body` 的类型是解码器的输出，所以解码器要写在 turbofish 里，不能靠接收处的类型标注推断。自定义解码（拆 `{code, msg, data}` 外壳、解密……）实现 `FromBytes`，`Output` 可以是拆出来的业务数据。
//!
//! 请求发出去之后失败（传输失败、非 2xx、响应解码失败）时，错误的 err_args 统一带 `method`、`url`、`status`（还没收到响应时是 `null`）；读完了 body 的，还带 `body` 原文，不渲染进对外消息。
//!
//! ## 重试
//!
//! `send` 的参数换成 `(&client, RetryCtx::new(次数, 策略))` 就带重试：没拿到解码成功的响应时，把这次的配置和失败信息（`SendFailure`：错误，以及传输失败的超时 / 建连标记，或者原始的 status、headers、body）交给策略。策略可以按这些信息改配置，返回 `RetryDecision`：
//!
//! - `Retry(cfg)`：确认可以重试，用这个配置再发；
//! - `Stop(err)`：立即结束，不管还剩几次；
//! - `Unknown(cfg)`：判断不了，原样再发，直到次数用完。
//!
//! `Retry` 和 `Unknown` 共用同一个次数上限。发出之前的错误（URL、params、header）不重试，直接返回。
//!
//! **只在两种情况下重试**：对方接口幂等（GET / PUT / DELETE，或者带了幂等键），或者根据错误调整请求后再发（401 换 token、签名过期重签……）。超时、读 body 中断时请求可能已经被对方执行了，不幂等的请求（下单、转账）不要原样重发。
//!
//! 重试要求请求体整个在内存里（`Json`、`Form`、`Raw`、`Multipart`，也就是实现了 `Clone` 的），解码器实现 `FromBytes`（body 读完再解码）；流式的 `RawStream`、reqwest 原生的 `multipart::Form`、`BodyStream` 开不了重试，编译期报错。
//!
//! ```ignore
//! use std::time::Duration;
//! use hygiea::http_client::reqwest_client::{Auth, FailStage, RequestConfig, RetryCtx, RetryDecision, SendFailure};
//!
//! type Cfg = RequestConfig<(), ()>;
//!
//! let resp = RequestConfig::plain(Method::GET, url)
//!     .auth(Auth::Bearer(token))
//!     .send::<Json<Profile>>((&client, RetryCtx::new(3, |attempt: usize, cfg: Cfg, f: SendFailure| async move {
//!         match &f.stage {
//!             // 401：刷新 token 后再发
//!             FailStage::Status(resp) if resp.status == 401 => match refresh_token().await {
//!                 Ok(token) => RetryDecision::Retry(cfg.auth(Auth::Bearer(token))),
//!                 Err(e) => RetryDecision::Stop(e),
//!             },
//!             // 4xx 是请求本身的问题，重试也没用
//!             FailStage::Status(resp) if resp.status.is_client_error() => RetryDecision::Stop(f.err),
//!             // 其他（超时、5xx……）：GET 幂等，退避后原样再发，直到次数用完
//!             _ => {
//!                 tokio::time::sleep(Duration::from_millis(100 << attempt)).await;
//!                 RetryDecision::Unknown(cfg)
//!             }
//!         }
//!     })))
//!     .await?;
//! ```
//!
//! ## debug 日志
//!
//! 测试里排查问题时，开 `debug-log` feature（facade 是 `http-client-reqwest-debug-log`，只放在 dev-dependencies 里），给 client 打开 `debug`：
//!
//! ```ignore
//! let client = ReqwestClient::debug().await?; // 默认配置加 debug，等于 ReqwestClient::from_config(&ReqwestConfig::default().debug(true), &RegistryConfig::default()).await
//! let client = ReqwestClient::from_config(&ReqwestConfig { timeout: Some(t), ..Default::default() }.debug(true), &RegistryConfig::default()).await?; // 要改别的配置时
//! // 配置文件里是 debug = true
//!
//! // 建好之后也能改，原地生效，这个 client 的所有 clone（包括 Resources 里那份）一起变，下一次请求起按新值打
//! client.set_debug(false);
//! client.set_pretty(true); // JSON 缩进成多行，默认关（一行）
//! ```
//!
//! 打开后不再打平时的单行日志，每次请求打两条，消息后面接一段 JSON（默认一行，`set_pretty(true)` 后另起一行缩进），无视 `enable_logging`，不打码：
//!
//! - `http call start`：`{method, url, request: {headers, params, body}}`
//! - 结束（`http call success` / `non-2xx` / `decode failed` / `failed`）：`{method, url, status, elapsed_ms, request, response: {headers, body}, error}`，带上完整的请求，单独一条就能看懂
//!
//! `url` 是真正发出去的地址；请求头是请求自己的头补上 client 默认头，敏感头也是原值；JSON 的 params、body、响应 body 嵌套成对象，别的是字符串。级别和平时一样（成功 INFO，失败按 `failed_log_level`）。返回给调用方的错误和 `HttpResponse.url` 仍是打码的。请求头是照 reqwest 的规则算的，发送时才加的 `Host`、`Content-Length`、`Accept-Encoding`、cookie、代理认证头不在里面；传输失败没有 `response`；`BodyStream` 成功时 `response.body` 为 null。
//!
//! ## 限制
//!
//! - 泛型函数里调用 `send`，params、body、resp 这几个泛型参数都要加 `Send` 约束：`IntoSender::send_request` 返回的 future 标了 `+ Send`，trait 返回的 `impl Future` 不会自动带出 `Send`，只能写在签名上
//! - 用闭包写重试策略时，参数类型要写全（比如 `cfg: RequestConfig<(), Json<X>>`），编译器没法从 `Retry` trait 反推
//! - 每次尝试前都 clone 一份配置，包括最后一次。`Json<T>` 的 clone 是 `T` 的深拷贝，body 很大时有开销；`Raw` / `Multipart` 是 `Bytes`，clone 只是引用计数加一
//! - `IntoSender::send_request` 外部也能调用（trait 方法的可见性跟着 trait 走），只是标了 `#[doc(hidden)]`；要彻底挡住得给它加一个私有类型的参数

mod body;
mod client;
mod config;
#[cfg(feature = "debug-log")]
mod debug;
mod error;
mod headers;
mod logging;
mod request;
mod response;
mod retry;
mod send;
mod text;

pub use body::{ContentType, Form, IntoBody, Json, Multipart, MultipartPart, Raw, RawStream};
pub use client::{ReqwestClient, ReqwestComponent, ReqwestConfig};
pub use config::{
    HeaderBytes, HeaderMapConfig, HeaderText, HeaderValueConfig, ProxyBasicAuth, ProxyConfig,
    ProxyKind,
};
pub use error::HttpClientErr;
pub use headers::{Auth, IntoHeaders};
pub use logging::FailedLogLevel;
pub use request::{HttpResponse, RequestConfig};
pub use response::{BodyStream, FromBody, FromBytes};
pub use retry::{Retry, RetryCtx, RetryDecision, RetryFuture};
pub use send::{FailStage, IntoSender, RespBody, SendFailure};

// ---------------------------- 再导出 ----------------------------

/// reqwest 整个再导出：`multipart`、从错误的 source 里 downcast 用的 `reqwest::Error` 等从这里取，
/// 调用方不必自己依赖 reqwest，也就不会撞上两个版本的同名类型对不上的问题。
/// reqwest 原生的 `Client` / `ClientBuilder` 不经过本模块的 API，`send` 只收 [`ReqwestClient`]
pub use reqwest;

/// 日常最常用的几个，给个短路径
pub use bytes::Bytes;
pub use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
pub use reqwest::{Body, Method, Proxy, RequestBuilder, Response, StatusCode, Url, multipart};

/// [`FailedLogLevel`] 转换的目标类型
pub use tracing::Level;
