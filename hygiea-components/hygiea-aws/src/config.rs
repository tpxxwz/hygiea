//! 各服务共用的部分：写死的凭证 [`StaticCredentials`]；开了 `sdk-env` 时按 region、endpoint、profile、凭证从 SDK 环境加载 `SdkConfig`。

use std::fmt;

use aws_credential_types::Credentials;
use serde::Deserialize;

/// 写死在配置里的凭证。`Debug` 不输出 secret 和 session token
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    /// 临时凭证（STS）才有，长期密钥不写
    #[serde(default)]
    pub session_token: Option<String>,
}

impl fmt::Debug for StaticCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticCredentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"***")
            .field("session_token", &self.session_token.as_ref().map(|_| "***"))
            .finish()
    }
}

impl StaticCredentials {
    /// SDK 的凭证类型
    // 没开任何服务 feature 时没有调用方
    #[cfg_attr(not(feature = "s3"), allow(dead_code))]
    pub(crate) fn to_sdk(&self) -> Credentials {
        Credentials::new(
            &self.access_key_id,
            &self.secret_access_key,
            self.session_token.clone(),
            None,
            "hygiea-config",
        )
    }
}

/// 按配置加载 `SdkConfig`，没给的项由 SDK 从环境加载：环境变量（`AWS_REGION`、`AWS_ACCESS_KEY_ID`、`AWS_ENDPOINT_URL` 等）、
/// `~/.aws/config` 和 `~/.aws/credentials`、容器和 EC2 的实例凭证。给了的项优先。
/// 环境里的凭证是用到时才解析的，所以这里不会因为凭证缺失而失败，第一次调用服务时才会报错
#[cfg(feature = "sdk-env")]
// 没开任何服务 feature 时没有调用方
#[cfg_attr(not(feature = "s3"), allow(dead_code))]
pub(crate) async fn load_sdk_config(
    region: Option<&str>,
    endpoint_url: Option<&str>,
    profile: Option<&str>,
    credentials: Option<&StaticCredentials>,
) -> aws_config::SdkConfig {
    use aws_config::{BehaviorVersion, Region};

    let mut loader = aws_config::defaults(BehaviorVersion::latest());
    if let Some(region) = region {
        loader = loader.region(Region::new(region.to_owned()));
    }
    if let Some(url) = endpoint_url {
        loader = loader.endpoint_url(url);
    }
    if let Some(profile) = profile {
        loader = loader.profile_name(profile);
    }
    if let Some(credentials) = credentials {
        loader = loader.credentials_provider(credentials.to_sdk());
    }
    loader.load().await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Debug 不输出 secret 和 session token
    #[test]
    fn debug_hides_secrets() {
        let creds = StaticCredentials {
            access_key_id: "ak".into(),
            secret_access_key: "sk-secret".into(),
            session_token: Some("token-secret".into()),
        };
        let out = format!("{creds:?}");
        assert!(out.contains("ak"));
        assert!(!out.contains("sk-secret"));
        assert!(!out.contains("token-secret"));
    }

    /// 给了的项优先于 SDK 环境
    #[cfg(feature = "sdk-env")]
    #[tokio::test]
    async fn load_applies_explicit_values() {
        let credentials = StaticCredentials {
            access_key_id: "ak".into(),
            secret_access_key: "sk".into(),
            session_token: None,
        };
        let sdk = load_sdk_config(
            Some("auto"),
            Some("https://example.com"),
            None,
            Some(&credentials),
        )
        .await;
        assert_eq!(sdk.region().map(|r| r.as_ref()), Some("auto"));
        assert_eq!(sdk.endpoint_url(), Some("https://example.com"));
        assert!(sdk.credentials_provider().is_some());
    }
}
