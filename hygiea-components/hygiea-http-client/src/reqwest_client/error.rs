//! http 模块的错误。和 [`BaseErr`](crate::BaseErr) 共用项目前缀 999，内部模块前缀是 101，错误码全局唯一

use hygiea_core::{HyErr, err, hy_err};

use super::{Method, StatusCode};

/// http 各阶段的错误，错误码按阶段分段：建 client `0xx`、发出之前 `10x`、发出之后 `20x`：
///
/// - 建 client：[`ClientBuildFailed`](Self::ClientBuildFailed)
/// - 发出之前：[`InvalidUrl`](Self::InvalidUrl)、[`InvalidParams`](Self::InvalidParams)、
///   [`InvalidHeader`](Self::InvalidHeader)、[`RequestBuildFailed`](Self::RequestBuildFailed)。
///   `send` 按这个顺序逐项检查，报的就是第一个有问题的部分；是调用方参数的问题，直接返回，不打失败日志
/// - 发出之后：[`RequestFailed`](Self::RequestFailed)、[`NonSuccessStatus`](Self::NonSuccessStatus)、
///   [`DecodeFailed`](Self::DecodeFailed)，打失败日志。这三个的 err_args 统一带 `method`、`url`、`status`
///   （还没收到响应时是 `null`），读完了 body 的（后两个）再带 `body`
///
/// 原始错误挂在 source 上，要判断超时、连接失败之类决定重试时直接 downcast：
/// `err.source().and_then(|e| e.downcast_ref::<reqwest::Error>())`
#[derive(hy_err)]
#[err_code_internal_module_prefix = "101"]
pub enum HttpClientErr {
    // ---- 建 client ----
    /// `ReqwestClient::from_config` 失败，比如 TLS 后端初始化失败、代理配置不合法；原始错误挂在 source 上
    #[error(err_code = "01", err_tpl = "Http client build failed")]
    ClientBuildFailed,

    // ---- 发出之前 ----
    /// URL 不能用：解析不了，或者不是带 host 的 http / https 地址
    #[error(err_code = "11", err_tpl = "Invalid url: {{ url }}")]
    InvalidUrl,
    /// query params 编码不了，比如嵌套值（urlencoded 不支持）
    #[error(
        err_code = "12",
        err_tpl = "Invalid query params: {{ method }} {{ url }}"
    )]
    InvalidParams,
    /// 请求头的名字或值不合法（比如认证值里有换行），或者用了保留字段
    #[error(err_code = "13", err_tpl = "Invalid header: {{ cause }}")]
    InvalidHeader,
    /// 前面几项都验证通过了，reqwest 构建请求时仍然失败。能确定的只有「没构建出来」，具体原因看 source；
    /// 最常见的是 `Form` body 没法 urlencoded 编码（比如嵌套值），`Json` body 在日志预览那一步就已经验证过了
    #[error(
        err_code = "14",
        err_tpl = "Http request build failed: {{ method }} {{ url }}"
    )]
    RequestBuildFailed,

    // ---- 发出之后 ----
    /// 请求发出去了但失败了：超时、连不上、TLS 握手失败、读 body 中断。读 body 中断时 `status` 有值
    #[error(
        err_code = "21",
        err_tpl = "Http request failed: {{ method }} {{ url }}"
    )]
    RequestFailed,
    /// 收到了非 2xx 响应；`body` 在 err_args 里，打日志可见，不渲染进对外消息
    #[error(err_code = "22", err_tpl = "Http {{ status }}: {{ method }} {{ url }}")]
    NonSuccessStatus,
    /// 2xx，body 也读完了，但解码成 `Resp` 失败（[`FromBytes::from_bytes`](super::FromBytes::from_bytes)
    /// 返回了 `Err`）。原来的错误挂在 source 上；`body` 在 err_args 里，不渲染进对外消息
    #[error(
        err_code = "24",
        err_tpl = "Http response decode failed: {{ method }} {{ url }}"
    )]
    DecodeFailed,

    /// 把流式 body 写进调用方给的 writer 时失败（磁盘满、没权限……），本地问题，不是请求失败；
    /// 原始的 io 错误挂在 source 上
    #[error(err_code = "23", err_tpl = "Write response body failed")]
    WriteFailed,
}

// ---------------- 构造函数 ----------------
//
// 每个变体一个，模块内部统一从这里构造：错误里带哪些参数、原始错误怎么挂到 source 上，都只在这里写一次。
// 不能合成一个带变体参数的函数，因为 `err!` 要求变体是常量表达式。
//
// reqwest 错误挂 source 前先 `without_url()`：URL 已经在模板参数里（而且是打码后的），
// 不去掉的话 `{:#}` 会再打一遍 reqwest 自带的原始 URL，params 原文就漏出去了

/// URL 不能用。解析失败时调用方再 `.with_source(e)` 挂上解析错误
pub(super) fn invalid_url(url: &str) -> HyErr {
    err!(HttpClientErr::InvalidUrl, url)
}

/// query params 编码不了。`url` 是调用方写的 url 字符串（不含 params）
pub(super) fn invalid_params(method: &Method, url: &str, e: reqwest::Error) -> HyErr {
    err!(HttpClientErr::InvalidParams, { "method": method.as_str(), "url": url })
        .with_source(e.without_url())
}

/// 请求头不合法。`cause` 会渲染进对外消息，不要放凭据之类的原文
pub(super) fn invalid_header(cause: impl Into<String>) -> HyErr {
    err!(HttpClientErr::InvalidHeader, cause.into())
}

/// 前面几项都验证通过、reqwest 构建请求时仍然失败
pub(super) fn request_build_failed(method: &Method, url: &str, e: reqwest::Error) -> HyErr {
    err!(HttpClientErr::RequestBuildFailed, { "method": method.as_str(), "url": url })
        .with_source(e.without_url())
}

/// 请求发出去了但失败了。还没收到响应头时 `status` 是 `None`
pub(super) fn request_failed(
    method: &Method,
    url: &str,
    status: Option<StatusCode>,
    e: reqwest::Error,
) -> HyErr {
    err!(HttpClientErr::RequestFailed, {
        "method": method.as_str(),
        "url": url,
        "status": status.map(|s| s.as_u16()),
    })
    .with_source(e.without_url())
}

/// 非 2xx。`body` 放在 err_args 里，打日志可见，不渲染进对外消息
/// `ReqwestClient::from_config` 失败
pub(super) fn client_build_failed(e: reqwest::Error) -> HyErr {
    err!(HttpClientErr::ClientBuildFailed).with_source(e)
}

/// 写 writer 失败，io 错误挂在 source 上
pub(super) fn write_failed(e: std::io::Error) -> HyErr {
    err!(HttpClientErr::WriteFailed).with_source(e)
}

pub(super) fn non_success_status(
    method: &Method,
    status: StatusCode,
    url: &str,
    body: &str,
) -> HyErr {
    err!(HttpClientErr::NonSuccessStatus, {
        "status": status.as_u16(),
        "method": method.as_str(),
        "url": url,
        "body": body,
    })
}

/// 2xx 但解码失败。`source` 是 `from_bytes` 返回的错误，`body` 和非 2xx 一样放 err_args
pub(super) fn decode_failed(
    method: &Method,
    status: StatusCode,
    url: &str,
    body: &str,
    source: HyErr,
) -> HyErr {
    err!(HttpClientErr::DecodeFailed, {
        "method": method.as_str(),
        "url": url,
        "status": status.as_u16(),
        "body": body,
    })
    .with_source(source)
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use test_support::http_server::closed_port_url;

    use super::*;

    /// 不需要真实 reqwest::Error 的几个构造函数：错误码和参数
    mod constructors {
        use super::*;

        #[test]
        fn invalid_url_carries_url_arg() {
            let err = invalid_url("not a url");
            assert!(err.is(HttpClientErr::InvalidUrl));
            assert_eq!(err.err_args()["url"], "not a url");
        }

        #[test]
        fn invalid_header_carries_cause() {
            let err = invalid_header("bad value");
            assert!(err.is(HttpClientErr::InvalidHeader));
            assert_eq!(err.err_args()["cause"], "bad value");
        }

        #[test]
        fn write_failed_has_io_source() {
            let err = write_failed(std::io::Error::other("disk full"));
            assert!(err.is(HttpClientErr::WriteFailed));
            assert!(
                err.source()
                    .and_then(|e| e.downcast_ref::<std::io::Error>())
                    .is_some()
            );
        }

        #[test]
        fn non_success_status_carries_status_method_url_body() {
            let err =
                non_success_status(&Method::POST, StatusCode::BAD_GATEWAY, "http://h/x", "body");
            assert!(err.is(HttpClientErr::NonSuccessStatus));
            assert_eq!(err.err_args()["status"], 502);
            assert_eq!(err.err_args()["method"], "POST");
            assert_eq!(err.err_args()["url"], "http://h/x");
            assert_eq!(err.err_args()["body"], "body");
        }

        /// 字段和 NonSuccessStatus 一致，from_bytes 的原错误挂在 source 上
        #[test]
        fn decode_failed_carries_fields_and_source() {
            let inner = err!(hygiea_core::BaseErr::JsonError, "bad json");
            let err = decode_failed(&Method::GET, StatusCode::OK, "http://h/x", "{", inner);
            assert!(err.is(HttpClientErr::DecodeFailed));
            assert_eq!(err.err_args()["status"], 200);
            assert_eq!(err.err_args()["method"], "GET");
            assert_eq!(err.err_args()["url"], "http://h/x");
            assert_eq!(err.err_args()["body"], "{");
            let source = err
                .source()
                .and_then(|e| e.downcast_ref::<HyErr>())
                .unwrap();
            assert!(source.is(hygiea_core::BaseErr::JsonError));
            assert!(!format!("{err}").contains('{'));
        }
    }

    /// `NonSuccessStatus` 的 body 只在 err_args 里，不出现在对外 Display（`{}` / `{:#}`）
    #[test]
    fn non_success_status_body_not_in_display() {
        let err = non_success_status(&Method::GET, StatusCode::NOT_FOUND, "http://h/", "leak-me");
        assert!(!format!("{err}").contains("leak-me"));
        assert!(!format!("{err:#}").contains("leak-me"));
    }

    /// 需要真实 `reqwest::Error` 的几个构造函数：source 挂上之前先 `without_url()`，
    /// `{:#}` 拼接 source 链时不会带出 reqwest 自己记录的 url（也就是 query 原文）
    mod hides_query_in_source {
        use super::*;

        /// 连一个关掉的端口拿到真实的 reqwest::Error，它的 url 里带着 query
        async fn reqwest_error_with_query() -> reqwest::Error {
            let base = closed_port_url().await;
            reqwest::get(format!("{base}/x?token=secret"))
                .await
                .unwrap_err()
        }

        #[tokio::test]
        async fn invalid_params() {
            let e = reqwest_error_with_query().await;
            let err = super::invalid_params(&Method::GET, "http://h/x", e);
            assert!(err.is(HttpClientErr::InvalidParams));
            let rendered = format!("{err:#}");
            assert!(!rendered.contains("token=secret"), "{rendered}");
        }

        #[tokio::test]
        async fn request_build_failed() {
            let e = reqwest_error_with_query().await;
            let err = super::request_build_failed(&Method::GET, "http://h/x", e);
            assert!(err.is(HttpClientErr::RequestBuildFailed));
            let rendered = format!("{err:#}");
            assert!(!rendered.contains("token=secret"), "{rendered}");
        }

        #[tokio::test]
        async fn request_failed() {
            let e = reqwest_error_with_query().await;
            let err = super::request_failed(&Method::GET, "http://h/x", None, e);
            assert!(err.is(HttpClientErr::RequestFailed));
            let rendered = format!("{err:#}");
            assert!(!rendered.contains("token=secret"), "{rendered}");
        }
    }

    /// `client_build_failed` 不是逐请求的错误，不会挂着带 query 的 url，所以不像上面几个
    /// 那样调用 `without_url()`；只需要确认错误码和 source 在
    #[tokio::test]
    async fn client_build_failed_carries_source() {
        let base = closed_port_url().await;
        let e = reqwest::get(base).await.unwrap_err();
        let err = client_build_failed(e);
        assert!(err.is(HttpClientErr::ClientBuildFailed));
        assert!(err.source().is_some());
    }
}
