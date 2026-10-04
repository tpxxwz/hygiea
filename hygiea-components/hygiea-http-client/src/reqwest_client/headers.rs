//! 请求头和认证：[`IntoHeaders`] 及保留字段校验、[`Auth`]

use std::fmt;

use hygiea_core::{Result, redact};

use super::error::invalid_header;
use super::{HeaderMap, HeaderValue};

// ============================ 请求头输入：IntoHeaders 与保留字段校验 ============================

/// 能作为请求头传给 [`RequestConfig::headers`](super::RequestConfig::headers) 的类型，转换失败返回 `HyErr`。
///
/// 内置实现：
/// - [`HeaderMap`] / `&HeaderMap`：一份请求头（同名多值用 `append` 追加）
/// - 多份请求头一起传：数组 `[T; N]`、`Vec<T>`，或者类型各不相同的元组 `(A, B)` ~ `(A, B, C, D)`。
///   按顺序合并，同名的以靠后的为准（整组值替换，不是追加），比如固定头在前、会话头在后：
///   `.headers([&*BASE_HEADERS, &session])`
///
/// 其他形式由外部类型自己实现这个 trait，比如签名头的结构体；保留字段统一由
/// [`RequestConfig::headers`](super::RequestConfig::headers) 拦截
pub trait IntoHeaders {
    fn into_headers(self) -> Result<HeaderMap>;
}

/// 已经是合法的 header，直接用
impl IntoHeaders for HeaderMap {
    fn into_headers(self) -> Result<HeaderMap> {
        Ok(self)
    }
}

/// 借用的 header，clone 一份（值是引用计数，不复制内容）
impl IntoHeaders for &HeaderMap {
    fn into_headers(self) -> Result<HeaderMap> {
        Ok(self.clone())
    }
}

/// 多份按顺序合并，同名的以靠后的为准
fn merge_headers<T: IntoHeaders>(parts: impl IntoIterator<Item = T>) -> Result<HeaderMap> {
    let mut merged = HeaderMap::new();
    for part in parts {
        // HeaderMap 的 extend 对已有的名字整组替换，后面的值覆盖前面的
        merged.extend(part.into_headers()?);
    }
    Ok(merged)
}

impl<T: IntoHeaders, const N: usize> IntoHeaders for [T; N] {
    fn into_headers(self) -> Result<HeaderMap> {
        merge_headers(self)
    }
}

impl<T: IntoHeaders> IntoHeaders for Vec<T> {
    fn into_headers(self) -> Result<HeaderMap> {
        merge_headers(self)
    }
}

/// 元组里的类型可以不同，比如 `(HeaderMap, &HeaderMap)`，规则同数组
macro_rules! impl_into_headers_for_tuple {
    ($($part:ident),+) => {
        impl<$($part: IntoHeaders),+> IntoHeaders for ($($part,)+) {
            #[allow(non_snake_case)]
            fn into_headers(self) -> Result<HeaderMap> {
                let ($($part,)+) = self;
                let mut merged = HeaderMap::new();
                $(merged.extend($part.into_headers()?);)+
                Ok(merged)
            }
        }
    };
}

impl_into_headers_for_tuple!(A, B);
impl_into_headers_for_tuple!(A, B, C);
impl_into_headers_for_tuple!(A, B, C, D);

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
pub(super) fn checked_headers(headers: impl IntoHeaders) -> Result<HeaderMap> {
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
    /// 拼成 `Authorization: Basic <base64(username:password)>`，没有密码时是 `base64(username:)`
    Basic {
        username: String,
        password: Option<String>,
    },
    /// 拼成 `Authorization: Bearer <token>`
    Bearer(String),
    /// 自定义 scheme，拼成 `Authorization: <scheme> <credentials>`。
    /// 交易所的 HMAC 签名、AWS 的 `AWS4-HMAC-SHA256` 这类都走这里
    Custom { scheme: String, credentials: String },
    /// 现成的整段值，原样写成 `Authorization: <value>`，不加任何 scheme
    Plain(String),
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
            Self::Plain(_) => f.debug_tuple("Plain").field(&masked).finish(),
        }
    }
}

/// 构造认证头的值并标成 sensitive，`Debug` 里显示 `Sensitive`，凭据不会漏进日志。
///
/// reqwest 没有暴露 header_sensitive，所以自己构造，再走公开的 `header()`——它只会把非敏感变敏感、
/// 不会反向关掉，标记能保住。值里有换行之类的非法字符时报 `InvalidHeader`，错误信息里不带凭据原文
pub(super) fn auth_value(raw: String) -> Result<HeaderValue> {
    let mut value = HeaderValue::from_str(&raw).map_err(|e| {
        invalid_header("authorization value contains invalid characters").with_source(e)
    })?;
    value.set_sensitive(true);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use crate::reqwest_client::BaseHttpErr;
    use test_support::headers::header_map;

    use super::*;

    /// 多份请求头一起传：按顺序合并，同名的以靠后的为准
    mod merge {
        use super::*;

        fn value(headers: &HeaderMap, name: &str) -> Vec<String> {
            headers
                .get_all(name)
                .iter()
                .map(|v| v.to_str().unwrap().to_owned())
                .collect()
        }

        /// 数组：不同名的都保留，同名的整组被后面的替换（不是追加）
        #[test]
        fn array_later_overrides_earlier() {
            let mut base = header_map(&[("x-a", "1"), ("x-b", "base")]);
            base.append("x-multi", HeaderValue::from_static("m1"));
            base.append("x-multi", HeaderValue::from_static("m2"));
            let session = header_map(&[("x-b", "session"), ("x-multi", "s"), ("x-c", "3")]);

            let merged = [&base, &session].into_headers().unwrap();
            assert_eq!(value(&merged, "x-a"), ["1"]);
            assert_eq!(value(&merged, "x-b"), ["session"]);
            assert_eq!(value(&merged, "x-multi"), ["s"]);
            assert_eq!(value(&merged, "x-c"), ["3"]);
        }

        /// 后面的一份里同名多值会整组保留
        #[test]
        fn later_multi_values_are_kept() {
            let base = header_map(&[("x-multi", "base")]);
            let mut later = HeaderMap::new();
            later.append("x-multi", HeaderValue::from_static("l1"));
            later.append("x-multi", HeaderValue::from_static("l2"));
            let merged = vec![base, later].into_headers().unwrap();
            assert_eq!(value(&merged, "x-multi"), ["l1", "l2"]);
        }

        /// 元组里类型可以不同，规则同数组
        #[test]
        fn tuple_of_mixed_types() {
            let base = header_map(&[("x-a", "1"), ("x-b", "base")]);
            let session = header_map(&[("x-b", "session")]);
            let extra = header_map(&[("x-c", "3")]);
            let merged = (&base, session, extra).into_headers().unwrap();
            assert_eq!(value(&merged, "x-a"), ["1"]);
            assert_eq!(value(&merged, "x-b"), ["session"]);
            assert_eq!(value(&merged, "x-c"), ["3"]);
        }

        /// 合并之后再过保留字段检查，任何一份里有保留字段都会被拦下
        #[test]
        fn reserved_header_in_any_part_is_rejected() {
            let base = header_map(&[("x-a", "1")]);
            let bad = header_map(&[("content-type", "text/plain")]);
            let err = checked_headers([&base, &bad]).unwrap_err();
            assert!(err.is(BaseHttpErr::InvalidHeader));
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
