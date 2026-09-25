//! 示范：给第三方常见的 `{code, msg, data}` 外层结构实现 `FromBody`，调用方在自己的项目里照这个写。
//!
//! HTTP 2xx 之后解析外层结构。业务码和消息是对方返回的数据，原样交给调用方判断，不转成 HyErr——
//! HyErr 只表示这次调用本身失败（网络、非 2xx、JSON 解析不了）。

use hygiea_core::net::http::*;
use hygiea_core::redact::{self, redact};
use hygiea_core::{BaseErr, HyErr, ResultExt, err};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::support::*;

// ============================ 示范代码 ============================

/// 要 Serialize：日志里打的是解码后重新序列化的结果，`T` 里标了 `#[redact(..)]` 的字段会打码
#[derive(Debug, Serialize)]
pub struct ApiResult<T> {
    pub code: String,
    pub msg: String,
    /// 只在 `code == "0"` 时解析成 `T`；业务失败时 `data` 往往为空或者结构不同，不解析
    pub data: Option<T>,
    /// 业务失败时没解析的原始 `data`，只给日志用
    #[serde(skip)]
    raw_data: serde_json::Value,
}

impl<T> ApiResult<T> {
    pub fn is_ok(&self) -> bool {
        self.code == "0"
    }
}

/// 先按原样收下 `data`，确认业务成功后再解析成 `T`。
/// 经 `Json<Envelope>` 解码，所以也要 Serialize（`Json<T>` 解码要求 `T` 能重新序列化）
#[derive(Deserialize, Serialize)]
struct Envelope {
    code: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    data: serde_json::Value,
}

impl<T: DeserializeOwned + Serialize> FromBody for ApiResult<T> {
    fn from_bytes(headers: &HeaderMap, body: Bytes) -> Result<Self, HyErr> {
        let Json(envelope) = Json::<Envelope>::from_bytes(headers, body)?;
        let (data, raw_data) = if envelope.code == "0" {
            let data = serde_json::from_value(envelope.data)
                .wrap_err(|| err!(BaseErr::JsonError, "deserialize response data failed"))?;
            (Some(data), serde_json::Value::Null)
        } else {
            (None, envelope.data)
        };
        Ok(Self {
            code: envelope.code,
            msg: envelope.msg,
            data,
            raw_data,
        })
    }

    /// 业务成功时按 `T` 的规则打码；业务失败时 `data` 没解析、没法打码，把 code、msg 和原始 data
    /// 原样打出来，和非 2xx 一样，排查为什么失败要看对方到底回了什么。
    /// 返回 `None` 的话成功日志里没有 resp，所以这里自己拼
    fn decoded_preview(&self) -> Option<String> {
        if !self.is_ok() {
            let raw =
                serde_json::json!({ "code": self.code, "msg": self.msg, "data": self.raw_data });
            return Some(raw.to_string());
        }
        Some(
            redact::to_redacted_json(self).unwrap_or_else(|e| format!("<log preview failed: {e}>")),
        )
    }
}

// ============================ 测试 ============================

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct AddressInfo {
    address: String,
    balance: String,
}

/// 泛型 T 里的字段自己标打码，ApiResult 本身不用标
#[redact]
#[derive(Debug, Deserialize, Serialize)]
struct ApiKey {
    name: String,
    #[redact(mask)]
    secret: String,
}

const ROUTES: &[(&str, u16, &str)] = &[
    (
        "/ok",
        200,
        r#"{"code":"0","msg":"","data":[{"address":"0xabc","balance":"1.5"}]}"#,
    ),
    (
        "/biz-err",
        200,
        r#"{"code":"50011","msg":"Rate limit reached","data":[]}"#,
    ),
    (
        "/bad-data",
        200,
        r#"{"code":"0","msg":"","data":{"unexpected":true}}"#,
    ),
    ("/server-err", 500, r#"{"code":"50000","msg":"oops"}"#),
    (
        "/keys",
        200,
        r#"{"code":"0","msg":"","data":[{"name":"a","secret":"secret-a"},{"name":"b","secret":"secret-b"}]}"#,
    ),
    (
        "/keys-biz-err",
        200,
        r#"{"code":"50001","msg":"no permission","data":[{"hint":"secret-hint"}]}"#,
    ),
];

async fn fetch<T: DeserializeOwned + Serialize>(
    base: &str,
    path: &str,
) -> Result<HttpResponse<ApiResult<T>>, HyErr> {
    RequestConfig::plain(Method::GET, format!("{base}{path}"))
        .send(&local_config().build().unwrap())
        .await
}

/// 解码：业务成功、业务失败、结构不对、HTTP 失败
mod decoding {
    use super::*;

    /// 业务成功：data 解析成 T
    #[tokio::test]
    async fn success_parses_data() {
        let base = serve_routes(ROUTES).await;
        let r = fetch::<Vec<AddressInfo>>(&base, "/ok").await.unwrap();
        assert!(r.body.is_ok());
        assert_eq!(
            r.body.data,
            Some(vec![AddressInfo {
                address: "0xabc".into(),
                balance: "1.5".into()
            }])
        );
    }

    /// 业务失败不是调用失败：照样 Ok，业务码和消息交给调用方
    #[tokio::test]
    async fn business_error_is_data_not_err() {
        let base = serve_routes(ROUTES).await;
        let r = fetch::<Vec<AddressInfo>>(&base, "/biz-err").await.unwrap();
        assert!(!r.body.is_ok());
        assert_eq!(r.body.code, "50011");
        assert_eq!(r.body.msg, "Rate limit reached");
        assert!(r.body.data.is_none());
    }

    /// 业务成功但 data 结构对不上：JsonError，serde 的具体原因挂在 source 上
    #[tokio::test]
    async fn data_shape_mismatch() {
        let base = serve_routes(ROUTES).await;
        let err = fetch::<Vec<AddressInfo>>(&base, "/bad-data")
            .await
            .unwrap_err();
        assert!(err.is(BaseErr::JsonError));
        assert!(format!("{err:#}").contains("invalid type"));
    }

    /// 非 2xx 先被 send 拦下，不会进 ApiResult 的解析
    #[tokio::test]
    async fn http_error_checked_before_business_code() {
        let base = serve_routes(ROUTES).await;
        let err = fetch::<Vec<AddressInfo>>(&base, "/server-err")
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::NonSuccessStatus));
        assert_eq!(err.err_args["status"], 500);
    }
}

/// 日志：业务成功时 data 里的字段打码，业务失败时打原文
mod logging {
    use super::*;

    /// 泛型 data 里标了的字段打码，其余字段照常；调用方拿到原文
    #[tokio::test]
    async fn masks_fields_inside_generic_data() {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        let r = fetch::<Vec<ApiKey>>(&base, "/keys").await.unwrap();
        assert_eq!(r.body.data.as_ref().unwrap()[1].secret, "secret-b");
        let log = out.text();
        assert!(log.contains("http call success"), "{log}");
        assert!(!log.contains("secret-"), "{log}");
        assert_eq!(log.matches(r#""secret":"***""#).count(), 2, "{log}");
        assert!(log.contains(r#""name":"a""#), "{log}");
    }

    /// 业务失败打原文，没解析的 data 也在
    #[tokio::test]
    async fn business_error_logs_raw_body() {
        let base = serve_routes(ROUTES).await;
        let (out, _guard) = capture();
        let r = fetch::<Vec<ApiKey>>(&base, "/keys-biz-err").await.unwrap();
        assert!(!r.body.is_ok());
        let log = out.text();
        assert!(log.contains("http call success"), "{log}");
        assert!(log.contains("secret-hint"), "{log}");
        assert!(log.contains("no permission"), "{log}");
    }
}
