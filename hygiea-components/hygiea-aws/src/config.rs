//! 公共配置 [`AwsConfig`]：所有服务共用的项，加载成 `SdkConfig`。

use std::fmt;

use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_credential_types::Credentials;
use serde::Deserialize;

#[cfg(feature = "s3")]
use crate::s3::S3Config;

/// `aws_config::SdkConfig` 的配置文件镜像。没写的项走 AWS 官方的默认链：
/// 环境变量（`AWS_REGION`、`AWS_ACCESS_KEY_ID`、`AWS_ENDPOINT_URL` 等）、`~/.aws/config` 和
/// `~/.aws/credentials`、容器和 EC2 的实例凭证。写了的项优先于默认链。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AwsConfig {
    /// 区域，比如 `us-east-1`；Cloudflare R2 填 `auto`。不写走默认链
    pub region: Option<String>,
    /// 所有服务共用的 endpoint，连兼容 S3 的第三方服务（R2、MinIO、七牛）时填。
    /// 不写走默认链，都没有就用 AWS 按 region 算出的官方地址
    pub endpoint_url: Option<String>,
    /// `~/.aws/config` 里的 profile 名，不写用 `AWS_PROFILE` 或 `default`
    pub profile: Option<String>,
    /// 写死的 AccessKey / SecretKey。写了就不走默认的凭证链
    pub credentials: Option<StaticCredentials>,
    /// S3 的专属配置，叠加在上面的公共配置之上
    #[cfg(feature = "s3")]
    pub s3: S3Config,
}

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

impl AwsConfig {
    /// 按配置加载 `SdkConfig`。默认链里的凭证是用到时才解析的，所以这里不会因为凭证缺失而失败，
    /// 第一次调用服务时才会报错
    pub async fn load(&self) -> SdkConfig {
        let mut loader = aws_config::defaults(BehaviorVersion::latest());
        if let Some(region) = &self.region {
            loader = loader.region(Region::new(region.clone()));
        }
        if let Some(url) = &self.endpoint_url {
            loader = loader.endpoint_url(url);
        }
        if let Some(profile) = &self.profile {
            loader = loader.profile_name(profile);
        }
        if let Some(c) = &self.credentials {
            loader = loader.credentials_provider(Credentials::new(
                &c.access_key_id,
                &c.secret_access_key,
                c.session_token.clone(),
                None,
                "hygiea-config",
            ));
        }
        loader.load().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置文件写法：公共项加上 `[s3]` 子表
    #[test]
    fn deserializes_from_toml() {
        let c: AwsConfig = toml::from_str(
            r#"
            region = "auto"
            endpoint_url = "https://example.r2.cloudflarestorage.com"
            credentials = { access_key_id = "ak", secret_access_key = "sk" }
            "#,
        )
        .unwrap();
        assert_eq!(c.region.as_deref(), Some("auto"));
        let creds = c.credentials.unwrap();
        assert_eq!(creds.access_key_id, "ak");
        assert_eq!(creds.session_token, None);
    }

    /// 拼错的项启动时报错，不被静默忽略
    #[test]
    fn rejects_unknown_fields() {
        assert!(toml::from_str::<AwsConfig>(r#"regoin = "x""#).is_err());
    }

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

    /// 写了的项优先于默认链
    #[tokio::test]
    async fn load_applies_explicit_values() {
        let c = AwsConfig {
            region: Some("auto".into()),
            endpoint_url: Some("https://example.com".into()),
            credentials: Some(StaticCredentials {
                access_key_id: "ak".into(),
                secret_access_key: "sk".into(),
                session_token: None,
            }),
            ..AwsConfig::default()
        };
        let sdk = c.load().await;
        assert_eq!(sdk.region().map(|r| r.as_ref()), Some("auto"));
        assert_eq!(sdk.endpoint_url(), Some("https://example.com"));
        assert!(sdk.credentials_provider().is_some());
    }
}
