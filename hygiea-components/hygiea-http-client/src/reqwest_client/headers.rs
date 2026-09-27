//! 请求头和认证：[`IntoHeaders`] 及保留字段校验、[`Auth`]

use std::fmt;

use hygiea_core::{HyErr, redact};

use super::error::invalid_header;
use super::{HeaderMap, HeaderValue};

// ============================ 请求头输入：IntoHeaders 与保留字段校验 ============================

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

/// client 和请求两级共用：转换并拦保留字段（`ReqwestConfig::default_headers`、`RequestConfig::headers`）
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

#[cfg(test)]
mod tests {
    use crate::reqwest_client::BaseHttpErr;
    use hygiea_test_support::headers::header_map;

    use super::*;

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
