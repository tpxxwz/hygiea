//! S3：专属配置 [`S3Config`]，资源 [`AwsS3Client`]（在公共的 `SdkConfig` 上叠加 S3 专属项后造出的
//! `aws_sdk_s3::Client`），组件 [`AwsS3Component`]。

use std::ops::Deref;

use aws_config::SdkConfig;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::{Builder, RequestChecksumCalculation, ResponseChecksumValidation};
use hygiea_core::Result;
use hygiea_core::app::{ConfigResource, ImmediateResourceComponent, async_trait};
use serde::Deserialize;

use crate::AwsConfig;

/// S3 专属项。没写的项继承公共配置或 SDK 默认值
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct S3Config {
    /// 只给 S3 用的 endpoint，覆盖公共的 `endpoint_url`。
    /// 公共配置连 AWS、只有 S3 连第三方服务时用
    pub endpoint_url: Option<String>,
    /// 用路径形式寻址（`https://host/bucket/key`），而不是把 bucket 放进域名
    /// （`https://bucket.host/key`）。MinIO、RustFS 这类自建服务一般要开，AWS 和 R2 不用
    pub force_path_style: bool,
    /// 什么时候给请求算校验和（CRC32 等）。不写用 SDK 默认的 `when_supported`；
    /// 不认新校验和头的第三方服务报错时改成 `when_required`
    pub request_checksum_calculation: Option<ChecksumWhen>,
    /// 什么时候校验响应的校验和。不写用 SDK 默认的 `when_supported`
    pub response_checksum_validation: Option<ChecksumWhen>,
}

/// 校验和的计算 / 校验时机，对应 SDK 的 `RequestChecksumCalculation` / `ResponseChecksumValidation`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumWhen {
    /// 操作支持就做
    WhenSupported,
    /// 只在操作要求时做
    WhenRequired,
}

impl S3Config {
    /// 在公共配置上叠加 S3 专属项，得到 SDK 的 config builder
    pub(crate) fn builder(&self, sdk_config: &SdkConfig) -> Builder {
        let mut builder = Builder::from(sdk_config);
        if let Some(url) = &self.endpoint_url {
            builder = builder.endpoint_url(url);
        }
        if self.force_path_style {
            builder = builder.force_path_style(true);
        }
        if let Some(when) = self.request_checksum_calculation {
            builder = builder.request_checksum_calculation(match when {
                ChecksumWhen::WhenSupported => RequestChecksumCalculation::WhenSupported,
                ChecksumWhen::WhenRequired => RequestChecksumCalculation::WhenRequired,
            });
        }
        if let Some(when) = self.response_checksum_validation {
            builder = builder.response_checksum_validation(match when {
                ChecksumWhen::WhenSupported => ResponseChecksumValidation::WhenSupported,
                ChecksumWhen::WhenRequired => ResponseChecksumValidation::WhenRequired,
            });
        }
        builder
    }
}

/// S3 客户端，`Deref` 到 `aws_sdk_s3::Client`，操作照常调。内部是 `Arc`，clone 很便宜，长期持有共享。
/// 用 [`ConfigResource::from_config`] 建，不经过 Registry 也能用
#[derive(Clone, Debug)]
pub struct AwsS3Client {
    inner: Client,
}

#[async_trait]
impl ConfigResource for AwsS3Client {
    type Config = AwsConfig;

    /// 先按公共项加载 `SdkConfig`，再叠加 [`AwsConfig::s3`]。默认链里的凭证用到时才解析，
    /// 所以这里不会因为凭证缺失而失败，第一次调用服务时才会报错
    async fn from_config(config: &AwsConfig) -> Result<Self> {
        let sdk_config = config.load().await;
        tracing::info!(
            "AWS config loaded [region={:?}]",
            sdk_config.region().map(|r| r.as_ref())
        );
        Ok(Self {
            inner: Client::from_conf(config.s3.builder(&sdk_config).build()),
        })
    }
}

impl Deref for AwsS3Client {
    type Target = Client;

    fn deref(&self) -> &Client {
        &self.inner
    }
}

/// S3 组件，按组件名放进 Resources 的是 [`AwsS3Client`]。用法见 [`ImmediateResourceComponent`]
pub type AwsS3Component = ImmediateResourceComponent<AwsS3Client>;

#[cfg(test)]
mod tests {
    use aws_config::{BehaviorVersion, Region};

    use super::*;

    fn sdk_config() -> SdkConfig {
        SdkConfig::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("auto"))
            .endpoint_url("https://shared.example.com")
            .build()
    }

    /// 没写的项继承公共配置
    #[test]
    fn inherits_shared_config() {
        let conf = S3Config::default().builder(&sdk_config()).build();
        assert_eq!(conf.region().map(|r| r.as_ref()), Some("auto"));
        assert_eq!(conf.request_checksum_calculation(), None);
    }

    /// 写了的项叠加上去
    #[test]
    fn overrides_apply() {
        let s3 = S3Config {
            endpoint_url: Some("https://s3-only.example.com".into()),
            force_path_style: true,
            request_checksum_calculation: Some(ChecksumWhen::WhenRequired),
            response_checksum_validation: Some(ChecksumWhen::WhenRequired),
        };
        let conf = s3.builder(&sdk_config()).build();
        assert_eq!(
            conf.request_checksum_calculation(),
            Some(&RequestChecksumCalculation::WhenRequired)
        );
        assert_eq!(
            conf.response_checksum_validation(),
            Some(&ResponseChecksumValidation::WhenRequired)
        );
    }

    /// 配置文件里的枚举值用 snake_case
    #[test]
    fn checksum_when_deserializes_snake_case() {
        let s3: S3Config =
            toml::from_str(r#"request_checksum_calculation = "when_required""#).unwrap();
        assert_eq!(
            s3.request_checksum_calculation,
            Some(ChecksumWhen::WhenRequired)
        );
    }
}
