//! 单次请求：配置 [`RequestConfig`] 和它的结果 [`HttpResponse`]。
//! 这里只放数据和转换（`into_request`），发请求的 `send` 在 send.rs；请求头、认证在 headers.rs，请求体在 body.rs

use std::time::Duration;

use reqwest::header::AUTHORIZATION;

use hygiea_core::HyErr;

use super::body::IntoBody;
use super::headers::{Auth, IntoHeaders, auth_value, checked_headers};
use super::logging::FailedLogLevel;
use super::{Bytes, Client, HeaderMap, Method, RequestBuilder, StatusCode, Url};

/// `RequestBuilder` 的纯数据镜像，字段顺序与方法声明顺序一致；本模块自己加的开关放在最后一段。
///
/// 只收 reqwest 里真正属于单请求的项。连接池、超时基线、重定向策略、代理、TLS 这些属于 `Client`，
/// 建好就固定了，改不了单次请求，都在 [`ReqwestConfig`](super::ReqwestConfig) 里。
///
/// 和 [`ReqwestConfig`](super::ReqwestConfig) 一样，下面的字段除非另有注明否则一律可用，不需要额外开 feature。
#[derive(Clone)]
pub struct RequestConfig<Params = (), Req = ()> {
    pub method: Method,
    pub url: String,

    /// 本次请求的头，同名会覆盖 client 的 `default_headers`；允许同名 header 出现多次。
    /// 通过 [`RequestConfig::headers`] 添加会校验保留字段，直接改这个字段则不校验
    pub headers: HeaderMap,
    /// Authorization 头，通过 [`RequestConfig::auth`] 选择一种认证方式，见 [`Auth`]
    pub auth: Option<Auth>,
    /// 请求体，见 [`IntoBody`]，`()` 表示没有
    pub body: Req,
    /// 覆盖 client 的整体超时，只对本次请求生效。
    /// 单请求级只有这一个超时旋钮——`RequestBuilder` 没有单请求版的 `read_timeout`，
    /// 要按连接改读空闲上限只能换 `Client`
    pub timeout: Option<Duration>,
    /// query 参数，不分请求方法一律拼到 URL 上，与 body 互不影响。
    /// 保留调用方的原始类型，发送时交给 reqwest 序列化；追加到 URL 已有 query 后。
    /// `()` 表示没有。日志里 `req` 的 params 段按 [`redact::to_redacted_json`](hygiea_core::redact::to_redacted_json) 打码；
    /// 日志、错误和 [`HttpResponse::url`] 里的地址也按同样的规则打码，发出去的仍是原文。
    ///
    /// 直接写在 `url` 字符串里的 query 没有结构、没法打码，会原样出现在日志里，
    /// 所以敏感参数要走 `params`，并在字段上标 `#[redact(..)]`
    pub params: Params,

    // ---- 以下是 reqwest 没有、本模块自己加的 ----
    /// 正常的请求/响应要不要打日志
    pub enable_logging: bool,
    /// 请求失败时用哪个级别打日志，见 [`FailedLogLevel`]
    pub failed_log_level: FailedLogLevel,
}

impl RequestConfig {
    /// 不带 params 和 body，等价于 `new(method, url, (), ())`
    pub fn plain(method: Method, url: impl Into<String>) -> Self {
        Self::new(method, url, (), ())
    }
}

impl<Params: serde::Serialize> RequestConfig<Params> {
    /// 只带 params，等价于 `new(method, url, params, ())`
    pub fn with_params(method: Method, url: impl Into<String>, params: Params) -> Self {
        Self::new(method, url, params, ())
    }
}

impl<Req: IntoBody> RequestConfig<(), Req> {
    /// 只带 body，等价于 `new(method, url, (), body)`
    pub fn with_body(method: Method, url: impl Into<String>, body: Req) -> Self {
        Self::new(method, url, (), body)
    }
}

impl<Params: serde::Serialize, Req: IntoBody> RequestConfig<Params, Req> {
    /// 一次给齐四项，没有的传 `()`：`RequestConfig::new(Method::GET, url, (), ())`。
    ///
    /// `params` 是 query 参数，可传实现 Serialize 的值或引用，不提前转换或编码；
    /// `body` 见 [`IntoBody`]，content-type 根据其类型自动设置。
    /// 两者的编码错误都由 reqwest 的 build() / send() 返回
    pub fn new(method: Method, url: impl Into<String>, params: Params, body: Req) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            auth: None,
            body,
            timeout: None,
            params,
            enable_logging: true,
            failed_log_level: FailedLogLevel::Warn,
        }
    }

    /// 添加请求头，见 [`IntoHeaders`]。多次调用、或者一次传数组 / 元组，都按顺序合并，同名的以靠后的为准
    /// （整组值替换，不是追加），比如 `.headers(&*BASE_HEADERS)?.headers(&session)?`。
    /// 转换失败或使用了保留字段时返回 `Err`；要清空已设置的请求头就重新构造一个 `RequestConfig`
    pub fn headers(mut self, headers: impl IntoHeaders) -> Result<Self, HyErr> {
        self.headers.extend(checked_headers(headers)?);
        Ok(self)
    }

    /// 设置本次请求的认证方式。重复调用会替换之前的设置，以最后一次为准。
    pub fn auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// 覆盖 client 的整体超时，只对本次请求生效。重复调用以最后一次为准
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// 正常的请求/响应要不要打日志。关掉后失败日志照样打
    pub fn enable_logging(mut self, enable: bool) -> Self {
        self.enable_logging = enable;
        self
    }

    /// 失败日志用哪个级别，见 [`FailedLogLevel`]
    pub fn failed_log_level(mut self, level: FailedLogLevel) -> Self {
        self.failed_log_level = level;
        self
    }

    /// 把本配置铺到 reqwest 的 `RequestBuilder` 上。
    ///
    /// 认证头的值在这里构造，值里有换行之类的非法字符时返回 [`InvalidHeader`](super::BaseHttpErr::InvalidHeader)；
    /// body 构造失败时返回 [`IntoBody::apply`] 报的错。
    /// URL、params、body 的错误和 reqwest 一样，要到 `build()` 时才暴露；`send` 会在那之前逐项检查
    pub fn into_request(self, client: &Client) -> Result<RequestBuilder, HyErr> {
        let mut req = client.request(self.method, &self.url).headers(self.headers);
        if let Some(auth) = self.auth {
            req = match auth {
                // base64 编码后总是合法的值，交给 reqwest
                Auth::Basic { username, password } => req.basic_auth(username, password),
                Auth::Bearer(token) => {
                    req.header(AUTHORIZATION, auth_value(format!("Bearer {token}"))?)
                }
                Auth::Custom {
                    scheme,
                    credentials,
                } => req.header(
                    AUTHORIZATION,
                    auth_value(format!("{scheme} {credentials}"))?,
                ),
            };
        }
        req = self.body.apply(req)?;
        if let Some(timeout) = self.timeout {
            req = req.timeout(timeout);
        }
        // () 序列化成 unit，reqwest 的 query 对它什么也不拼
        Ok(req.query(&self.params))
    }
}

// ============================ 响应 ============================

/// 一次请求的响应。能拿到这个结构就说明**确实收到了响应**——传输层失败（超时、连不上、TLS 握手
/// 失败）在发请求那一步就返回 `Err` 了，所以 `status` 不是 `Option`。
///
/// `Resp` 是 body 的类型，由 [`RequestConfig::send`] 按 [`FromBody`](super::FromBody) 得到；
/// 要原样的字节就用 [`Bytes`](super::Bytes)
#[derive(Debug, Clone)]
pub struct HttpResponse<Resp = Bytes> {
    /// 请求方法，非 2xx 报错时用
    pub method: Method,
    /// HTTP 状态码。`as_u16()` 取数字
    pub status: StatusCode,
    /// 响应头，名字大小写不敏感。取单个文本值用 [`HttpResponse::header`]
    pub headers: HeaderMap,
    /// 实际发出的请求地址，params 里标了 `#[redact(..)]` 的字段已经打码，和日志、错误里的一致，
    /// 可以放心打印。跟随了重定向时仍是最初发出的地址，不是最终跳到的地址
    pub url: Url,
    /// 发出请求到 body 交给调用方的耗时，和日志里的 `elapsed_ms` 是同一个值：
    /// 读完整个 body 的类型（`Json`、`Bytes`……）算到读完为止，[`BodyStream`](super::BodyStream) 算到收到响应头为止
    pub elapsed: Duration,
    pub body: Resp,
}

impl<Resp> HttpResponse<Resp> {
    /// 2xx
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// 取单个响应头的文本值；没有这个头，或者值不是合法文本时返回 `None`。同名多值时取第一个
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

#[cfg(test)]
mod tests {
    use crate::reqwest_client::BaseHttpErr;
    use crate::reqwest_client::{HeaderValue, Json};
    use hygiea_core::redact::redact;
    use hygiea_test_support::headers::header_map;
    use serde::Serialize;
    use serde_json::{Value, json};

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

    /// 构建出最终的 reqwest::Request，看真正要发出去的东西
    fn build<P: Serialize, B: IntoBody>(cfg: RequestConfig<P, B>) -> reqwest::Request {
        cfg.into_request(&Client::new()).unwrap().build().unwrap()
    }

    fn auth_header(req: &reqwest::Request) -> &HeaderValue {
        req.headers().get(AUTHORIZATION).unwrap()
    }

    /// 几个构造函数和默认值
    mod constructors {
        use super::*;

        /// `new` 之后除了四个入参，其余都是默认值：没有额外的头和认证、不覆盖超时、
        /// 正常日志打开、失败日志用 Warn
        #[test]
        fn new_fills_defaults() {
            let cfg = RequestConfig::new(Method::PUT, URL, (), ());
            assert_eq!(cfg.method, Method::PUT);
            assert_eq!(cfg.url, URL);
            assert!(cfg.headers.is_empty());
            assert!(cfg.auth.is_none());
            assert_eq!(cfg.timeout, None);
            assert!(cfg.enable_logging);
            assert_eq!(cfg.failed_log_level, FailedLogLevel::Warn);
        }

        /// `plain` / `with_params` / `with_body` 只是 `new` 的简写，没给的那项是 `()`
        #[test]
        fn shorthands_leave_missing_parts_empty() {
            let plain = RequestConfig::plain(Method::GET, URL);
            assert_eq!((plain.params, plain.body), ((), ()));

            let params = RequestConfig::with_params(Method::GET, URL, [("a", "1")]);
            assert_eq!((params.params, params.body), ([("a", "1")], ()));

            let body = RequestConfig::with_body(Method::POST, URL, Json(1));
            assert_eq!((body.params, body.body), ((), Json(1)));
        }
    }

    /// 链式设置项
    mod setters {
        use super::*;

        /// 多次调用按顺序合并：不同名的都保留，同名的以后一次为准
        #[test]
        fn headers_merge_across_calls() {
            let cfg = RequestConfig::plain(Method::GET, URL)
                .headers(header_map(&[("x-a", "1"), ("x-b", "old")]))
                .unwrap()
                .headers(header_map(&[("x-b", "new"), ("x-c", "3")]))
                .unwrap();
            assert_eq!(cfg.headers["x-a"], "1");
            assert_eq!(cfg.headers.get_all("x-b").iter().count(), 1);
            assert_eq!(cfg.headers["x-b"], "new");
            assert_eq!(cfg.headers["x-c"], "3");
        }

        /// auth 重复调用以最后一次为准
        #[test]
        fn auth_replaces_previous_call() {
            let cfg = RequestConfig::plain(Method::GET, URL)
                .auth(Auth::Bearer("a".into()))
                .auth(Auth::Bearer("b".into()));
            assert!(matches!(cfg.auth, Some(Auth::Bearer(ref t)) if t == "b"));
        }

        /// 超时和两个日志开关
        #[test]
        fn timeout_and_logging_switches() {
            let cfg = RequestConfig::plain(Method::GET, URL)
                .timeout(Duration::from_millis(300))
                .enable_logging(false)
                .failed_log_level(FailedLogLevel::Error);
            assert_eq!(cfg.timeout, Some(Duration::from_millis(300)));
            assert!(!cfg.enable_logging);
            assert_eq!(cfg.failed_log_level, FailedLogLevel::Error);
        }
    }

    /// `RequestConfig::headers` 也拦保留字段（检查本身的测试在 headers.rs）
    mod reserved_headers {
        use super::*;

        /// `RequestConfig::headers` 走的也是这套检查
        #[test]
        fn request_headers_use_the_same_check() {
            let result = RequestConfig::plain(Method::GET, URL)
                .headers(header_map(&[("authorization", "Bearer x")]));
            assert!(result.is_err());
        }
    }

    /// `into_request()`：各项配置铺到 reqwest 请求上
    mod into_request {
        use super::*;

        /// 方法和地址原样带上
        #[test]
        fn method_and_url() {
            let req = build(RequestConfig::plain(Method::DELETE, URL));
            assert_eq!(req.method(), Method::DELETE);
            assert_eq!(req.url().as_str(), URL);
        }

        /// 自定义请求头带上
        #[test]
        fn headers_are_applied() {
            let cfg = RequestConfig::plain(Method::GET, URL)
                .headers(header_map(&[("x-a", "1")]))
                .unwrap();
            assert_eq!(build(cfg).headers()["x-a"], "1");
        }

        /// params 追加在 URL 已有的 query 后面，不是覆盖
        #[test]
        fn params_append_to_existing_query() {
            let cfg =
                RequestConfig::with_params(Method::GET, "http://127.0.0.1/p?a=1", [("b", "2")]);
            assert_eq!(build(cfg).url().query(), Some("a=1&b=2"));
        }

        /// params 是 `()` 时 URL 不变，也不会多出一个空的 `?`
        #[test]
        fn unit_params_leave_url_untouched() {
            let req = build(RequestConfig::plain(Method::GET, URL));
            assert_eq!(req.url().query(), None);
        }

        /// 结构体 params 按 urlencoded 编码，特殊字符转义，发的是原文不打码
        #[test]
        fn struct_params_are_urlencoded_unmasked() {
            let req = build(RequestConfig::with_params(Method::GET, URL, &LOGIN));
            assert_eq!(
                req.url().query(),
                Some("username=alice&password=p%40ss&device_id=dev-1")
            );
        }

        /// 单请求超时设到 reqwest 请求上
        #[test]
        fn timeout_is_applied() {
            let cfg = RequestConfig::plain(Method::GET, URL).timeout(Duration::from_secs(9));
            assert_eq!(build(cfg).timeout(), Some(&Duration::from_secs(9)));
        }

        /// body 经 IntoBody 铺上去，发原文
        #[test]
        fn body_is_applied_unmasked() {
            let req = build(RequestConfig::with_body(Method::POST, URL, Json(&LOGIN)));
            let body = req.body().unwrap().as_bytes().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(std::str::from_utf8(body).unwrap()).unwrap(),
                json!({"username":"alice","password":"p@ss","device_id":"dev-1"})
            );
        }
    }

    /// 认证方式：拼成什么 Authorization 头，以及凭据会不会漏进 Debug
    mod auth {
        use super::*;

        fn with_auth(auth: Auth) -> reqwest::Request {
            build(RequestConfig::plain(Method::GET, URL).auth(auth))
        }

        /// Basic：`用户名:密码` 做 base64，值标成 sensitive
        #[test]
        fn basic_with_password() {
            let req = with_auth(Auth::Basic {
                username: "alice".into(),
                password: Some("pw".into()),
            });
            let value = auth_header(&req);
            assert_eq!(value, "Basic YWxpY2U6cHc=");
            assert!(value.is_sensitive());
        }

        /// Basic 不带密码：冒号后面为空
        #[test]
        fn basic_without_password() {
            let req = with_auth(Auth::Basic {
                username: "alice".into(),
                password: None,
            });
            assert_eq!(auth_header(&req), "Basic YWxpY2U6");
        }

        /// Bearer：`Bearer <token>`，sensitive
        #[test]
        fn bearer() {
            let req = with_auth(Auth::Bearer("tok".into()));
            let value = auth_header(&req);
            assert_eq!(value, "Bearer tok");
            assert!(value.is_sensitive());
        }

        /// 自定义 scheme：`<scheme> <credentials>`，同样 sensitive
        #[test]
        fn custom_scheme() {
            let req = with_auth(Auth::Custom {
                scheme: "HMAC".into(),
                credentials: "k=1,sig=abc".into(),
            });
            let value = auth_header(&req);
            assert_eq!(value, "HMAC k=1,sig=abc");
            assert!(value.is_sensitive());
        }

        /// Bearer / Custom 的值里有换行：into_request 直接报 InvalidHeader，错误信息里不带凭据原文
        #[test]
        fn invalid_chars_are_invalid_header_without_leaking() {
            let cases = [
                Auth::Bearer("secret\ntoken".into()),
                Auth::Custom {
                    scheme: "X".into(),
                    credentials: "secret\nvalue".into(),
                },
            ];
            for auth in cases {
                let err = RequestConfig::plain(Method::GET, URL)
                    .auth(auth)
                    .into_request(&Client::new())
                    .unwrap_err();
                assert!(err.is(BaseHttpErr::InvalidHeader));
                let msg = format!("{err:#}");
                assert!(!msg.contains("secret"), "{msg}");
            }
        }

        /// Basic 是 base64 编码，用户名、密码里有换行也是合法的值
        #[test]
        fn basic_is_always_valid() {
            let req = with_auth(Auth::Basic {
                username: "a\nb".into(),
                password: Some("p\nq".into()),
            });
            assert!(auth_header(&req).to_str().unwrap().starts_with("Basic "));
        }
    }

    /// `HttpResponse`：手工拼的响应，不走网络
    mod response {
        use super::*;

        /// 手工拼一个响应，不走网络
        fn resp(status: u16, body: &'static [u8]) -> HttpResponse {
            HttpResponse {
                method: Method::GET,
                status: StatusCode::from_u16(status).unwrap(),
                headers: HeaderMap::new(),
                url: Url::parse("http://127.0.0.1/r").unwrap(),
                elapsed: Duration::from_millis(12),
                body: Bytes::from_static(body),
            }
        }

        /// `HttpResponse` 的查询方法
        mod accessors {
            use super::*;

            /// 只有 2xx 算成功，1xx / 3xx / 4xx / 5xx 都不算
            #[test]
            fn is_success_only_for_2xx() {
                for code in [200, 201, 204, 299] {
                    assert!(resp(code, b"").is_success(), "{code}");
                }
                for code in [101, 301, 304, 400, 404, 500, 503] {
                    assert!(!resp(code, b"").is_success(), "{code}");
                }
            }

            /// 响应头按名字取，大小写不敏感；没有这个头返回 `None`
            #[test]
            fn header_lookup_is_case_insensitive() {
                let mut r = resp(200, b"");
                r.headers
                    .insert("x-request-id", HeaderValue::from_static("abc"));
                assert_eq!(r.header("X-Request-Id"), Some("abc"));
                assert_eq!(r.header("x-missing"), None);
            }

            /// 值不是合法文本时返回 `None`，而不是 panic
            #[test]
            fn header_with_non_text_value_is_none() {
                let mut r = resp(200, b"");
                r.headers
                    .insert("x-bin", HeaderValue::from_bytes(b"\xff").unwrap());
                assert_eq!(r.header("x-bin"), None);
            }

            /// 同名多值时取第一个
            #[test]
            fn header_returns_first_of_many() {
                let mut r = resp(200, b"");
                r.headers.append("x-v", HeaderValue::from_static("1"));
                r.headers.append("x-v", HeaderValue::from_static("2"));
                assert_eq!(r.header("x-v"), Some("1"));
            }
        }
    }
}
