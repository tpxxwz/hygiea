//! HTTP 客户端的 reqwest 实现：reqwest 的一层薄封装。
//!
//! | 文件 | 内容 |
//! |---|---|
//! | client.rs | [`ReqwestConfig`]：client 级配置，造出长期持有、共享连接池的 [`Client`]；应用组件 [`ReqwestComponent`] |
//! | config.rs | 配置文件里用的类型：[`HeaderMapConfig`]、[`ProxyConfig`] 等 |
//! | request.rs | [`RequestConfig`]：单次请求的配置（纯数据）；[`HttpResponse`]：结果 |
//! | headers.rs | 请求头 [`IntoHeaders`] 及保留字段校验、认证 [`Auth`] |
//! | body.rs | 请求体 [`IntoBody`]：`Json` / `Form` / `Raw` / `Multipart`（可以重试）、`RawStream` / `multipart::Form`（流式） |
//! | response.rs | 响应体：[`FromBytes`]（读完再解码，可以重试）、[`FromBody`]（总入口）、[`BodyStream`]（流式） |
//! | send.rs | 发送流程：`send`、发出之前的检查、读响应、失败信息 [`SendFailure`]、`send` 的参数 [`IntoSender`]。流程图在它开头 |
//! | retry.rs | 重试：[`RetryCtx`]、[`Retry`]、[`RetryDecision`]，以及什么请求可以重试 |
//! | logging.rs | 日志：[`FailedLogLevel`]，start / success / failed 三种日志 |
//! | text.rs | 字节转成日志 / 错误里的文本（charset、控制字符），调用链在它开头 |
//! | error.rs | [`BaseHttpErr`]：各阶段的错误 |
//!
//! 依赖方向：`retry → send → request / headers / body / response / logging / text / error`，`client → config`。
//! `send` 和 `response` 互相引用：`FromBody::from_body` 的参数 [`RespBody`] 绑定着正在进行的那次发送。
//!
//! 请求和响应日志里的字段打码走 [`hygiea_core::redact`]。每次请求默认打 `http call start` / `http call success`
//! 两条 info 日志，失败（传输失败、非 2xx、解码失败）按 [`FailedLogLevel`] 打一条。

mod body;
mod client;
mod config;
mod error;
mod headers;
mod logging;
mod request;
mod response;
mod retry;
mod send;
mod text;

pub use body::{ContentType, Form, IntoBody, Json, Multipart, MultipartPart, Raw, RawStream};
pub use client::{ReqwestComponent, ReqwestConfig};
pub use config::{
    HeaderBytes, HeaderMapConfig, HeaderText, HeaderValueConfig, ProxyBasicAuth, ProxyConfig,
    ProxyKind,
};
pub use error::BaseHttpErr;
pub use headers::{Auth, IntoHeaders};
pub use logging::FailedLogLevel;
pub use request::{HttpResponse, RequestConfig};
pub use response::{BodyStream, FromBody, FromBytes};
pub use retry::{Retry, RetryCtx, RetryDecision, RetryFuture};
pub use send::{FailStage, IntoSender, RespBody, SendFailure};

// ---------------------------- 再导出 ----------------------------

/// reqwest 整个再导出。本模块刻意没收的配置（mTLS 的 `Certificate` / `Identity`、
/// `retry::Builder`、自定义 `redirect::Policy` 等）都从这里取，调用方不必自己依赖 reqwest，
/// 也就不会撞上两个版本的同名类型对不上的问题
pub use reqwest;

/// 日常最常用的几个，给个短路径
pub use bytes::Bytes;
pub use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
pub use reqwest::{
    Body, Client, ClientBuilder, Method, Proxy, RequestBuilder, Response, StatusCode, Url,
    multipart,
};

/// [`FailedLogLevel`] 转换的目标类型
pub use tracing::Level;
