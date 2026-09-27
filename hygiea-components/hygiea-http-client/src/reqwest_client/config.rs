//! 配置文件里用的类型：请求头 [`HeaderMapConfig`]、代理 [`ProxyConfig`]，反序列化后转成 reqwest 的类型。
//! 只给 [`ReqwestConfig`](super::ReqwestConfig) 用

use serde::Deserialize;

use hygiea_core::app::BaseAppErr;
use hygiea_core::{HyErr, err};

use super::error::client_build_failed;
use super::{HeaderMap, HeaderName, HeaderValue, Proxy};

#[derive(Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct HeaderMapConfig(std::collections::BTreeMap<String, HeaderValues>);

impl HeaderMapConfig {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for HeaderMapConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut map = f.debug_map();
        for (name, values) in &self.0 {
            let count = match values {
                HeaderValues::One(_) => 1,
                HeaderValues::Many(values) => values.len(),
            };
            map.entry(name, &count);
        }
        map.finish()
    }
}

impl From<HeaderMap> for HeaderMapConfig {
    fn from(headers: HeaderMap) -> Self {
        let mut values = std::collections::BTreeMap::<String, Vec<HeaderValueConfig>>::new();
        for (name, value) in headers.iter() {
            let entry = match value.to_str() {
                Ok(text) => HeaderValueConfig::DetailedText(HeaderText {
                    value: text.to_owned(),
                    sensitive: value.is_sensitive(),
                }),
                Err(_) => HeaderValueConfig::DetailedBytes(HeaderBytes {
                    bytes: value.as_bytes().to_vec(),
                    sensitive: value.is_sensitive(),
                }),
            };
            values
                .entry(name.as_str().to_owned())
                .or_default()
                .push(entry);
        }
        Self(
            values
                .into_iter()
                .map(|(name, values)| (name, HeaderValues::Many(values)))
                .collect(),
        )
    }
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum HeaderValues {
    One(HeaderValueConfig),
    Many(Vec<HeaderValueConfig>),
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum HeaderValueConfig {
    Text(String),
    DetailedText(HeaderText),
    DetailedBytes(HeaderBytes),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderText {
    pub value: String,
    #[serde(default)]
    pub sensitive: bool,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderBytes {
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub sensitive: bool,
}

impl TryFrom<HeaderValueConfig> for HeaderValue {
    type Error = reqwest::header::InvalidHeaderValue;

    fn try_from(config: HeaderValueConfig) -> Result<Self, Self::Error> {
        let (mut value, sensitive) = match config {
            HeaderValueConfig::Text(text) => (Self::from_str(&text)?, false),
            HeaderValueConfig::DetailedText(entry) => {
                (Self::from_str(&entry.value)?, entry.sensitive)
            }
            HeaderValueConfig::DetailedBytes(entry) => {
                (Self::from_bytes(&entry.bytes)?, entry.sensitive)
            }
        };
        value.set_sensitive(sensitive);
        Ok(value)
    }
}

impl TryFrom<HeaderMapConfig> for HeaderMap {
    type Error = String;

    fn try_from(config: HeaderMapConfig) -> Result<Self, Self::Error> {
        let mut headers = Self::new();
        for (name, values) in config.0 {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| e.to_string())?;
            let entries = match values {
                HeaderValues::One(entry) => vec![entry],
                HeaderValues::Many(entries) => entries,
            };
            for entry in entries {
                headers
                    .try_append(
                        name.clone(),
                        HeaderValue::try_from(entry).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(headers)
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    #[serde(default)]
    pub kind: ProxyKind,
    pub url: String,
    pub basic_auth: Option<ProxyBasicAuth>,
    pub custom_http_auth: Option<HeaderValueConfig>,
    pub headers: Option<HeaderMapConfig>,
    pub no_proxy: Option<String>,
}

impl ProxyConfig {
    pub fn all(url: impl Into<String>) -> Self {
        Self::new(ProxyKind::All, url)
    }

    pub fn http(url: impl Into<String>) -> Self {
        Self::new(ProxyKind::Http, url)
    }

    pub fn https(url: impl Into<String>) -> Self {
        Self::new(ProxyKind::Https, url)
    }

    fn new(kind: ProxyKind, url: impl Into<String>) -> Self {
        Self {
            kind,
            url: url.into(),
            basic_auth: None,
            custom_http_auth: None,
            headers: None,
            no_proxy: None,
        }
    }
}

impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("kind", &self.kind)
            .field("url", &"<redacted>")
            .field("basic_auth", &self.basic_auth.is_some())
            .field("custom_http_auth", &self.custom_http_auth.is_some())
            .field("headers", &self.headers.is_some())
            .field("no_proxy", &self.no_proxy)
            .finish()
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyBasicAuth {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    #[default]
    All,
    Http,
    Https,
}

impl TryFrom<ProxyConfig> for Proxy {
    type Error = HyErr;

    fn try_from(config: ProxyConfig) -> Result<Self, Self::Error> {
        let mut proxy = match config.kind {
            ProxyKind::All => Self::all(&config.url),
            ProxyKind::Http => Self::http(&config.url),
            ProxyKind::Https => Self::https(&config.url),
        }
        .map_err(client_build_failed)?;
        if let Some(auth) = config.basic_auth {
            proxy = proxy.basic_auth(&auth.username, &auth.password);
        }
        if let Some(value) = config.custom_http_auth {
            let value = HeaderValue::try_from(value)
                .map_err(|e| err!(BaseAppErr::InvalidConfig, format!("proxy auth: {e}")))?;
            proxy = proxy.custom_http_auth(value);
        }
        if let Some(headers) = config.headers {
            let headers = HeaderMap::try_from(headers)
                .map_err(|e| err!(BaseAppErr::InvalidConfig, format!("proxy headers: {e}")))?;
            proxy = proxy.headers(headers);
        }
        if let Some(no_proxy) = config.no_proxy {
            proxy = proxy.no_proxy(reqwest::NoProxy::from_string(&no_proxy));
        }
        Ok(proxy)
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    mod header_map_config {
        use super::*;

        #[test]
        fn native_headers_keep_multiple_values_and_sensitive_flag() {
            let mut headers = HeaderMap::new();
            headers.append("x-repeat", HeaderValue::from_static("one"));
            headers.append("x-repeat", HeaderValue::from_static("two"));
            let mut secret = HeaderValue::from_static("secret");
            secret.set_sensitive(true);
            headers.insert("x-secret", secret);

            let restored = HeaderMap::try_from(HeaderMapConfig::from(headers)).unwrap();
            let values: Vec<_> = restored.get_all("x-repeat").iter().collect();
            assert_eq!(values, vec!["one", "two"]);
            assert!(restored["x-secret"].is_sensitive());
        }
    }
}
