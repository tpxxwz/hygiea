//! HTTP 客户端：reqwest 的一层薄封装。
//!
//! - [`ClientConfig`]：client 级配置，造出长期持有、共享连接池的 [`Client`]
//! - [`RequestConfig`] / [`HttpResponse`]（request.rs）：单次请求和它的结果。`send` 要求 2xx 并按 [`FromBody`] 解码，要原样字节用 `Bytes`，大文件用 [`BodyStream`] 流式读
//! - 组成部分（parts.rs）：请求头 [`IntoHeaders`]、认证 [`Auth`]、请求体 [`IntoBody`]、响应体 [`FromBody`]
//! - [`BaseHttpErr`]：各阶段的错误
//! - 请求和响应日志里的字段打码走 [`crate::redact`]
//!
//! 每次请求默认打 `http call start` / `http call success` 两条 info 日志，
//! 失败（构建失败、传输失败、非 2xx、解码失败）按 [`FailedLogLevel`] 打一条。

mod client;
mod error;
mod logging;
mod parts;
mod request;

pub use client::ClientConfig;
pub use error::BaseHttpErr;
pub use logging::FailedLogLevel;
pub use parts::{
    Auth, BodyStream, ContentType, Form, FromBody, IntoBody, IntoHeaders, Json, Raw, RespBody,
};
pub use request::{HttpResponse, RequestConfig};

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
