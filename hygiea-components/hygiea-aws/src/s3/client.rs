//! [`AwsS3Client`]：一个客户端（服务地址 + 凭证），`Deref` 到 `aws_sdk_s3::Client`，只在组件内部用，bucket 句柄拿着它发请求。

use std::ops::Deref;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::{AppName, BehaviorVersion, Builder, Region};
use aws_sdk_s3::types::BucketLocationConstraint;
use hygiea_core::app::{AppErr, RegistryConfig};
use hygiea_core::{HyErr, Result, bail, ensure, err, impossible};

use super::{S3ClientConfig, S3Region};

/// S3 客户端，`Deref` 到 `aws_sdk_s3::Client`，SDK 的操作照常调。内部是 `Arc`，clone 很便宜
#[derive(Clone, Debug)]
pub(super) struct AwsS3Client {
    inner: Client,
    /// [`S3ClientConfig::presign_public_origin`]，检查过（`scheme://host[:port]`，结尾可以有 `/`）
    presign_public_origin: Option<String>,
    /// 配置里的客户端名，删 bucket 报错时用
    #[cfg_attr(not(feature = "s3-bucket-admin"), allow(dead_code))]
    name: String,
    /// 建 bucket 时决定要不要带 LocationConstraint
    region: Option<S3Region>,
    /// 能不能管理 bucket，见 [`S3ClientConfig::manage_buckets`]
    manage_buckets: bool,
}

impl AwsS3Client {
    /// 按客户端配置建，`name` 是客户端名，报错时带上。两种方式见 [`S3ClientConfig`]：
    /// 不用 SDK 环境时少了 `region`、`credentials`，或者写了 `profile`，以及设了 `use_sdk_env` 却没开 `sdk-env` feature，
    /// 报 `AppErr::InvalidConfig`。不发请求；SDK 环境里的凭证用到时才解析，缺了要第一次调用服务时才报错。
    /// 应用名（`registry_config.app_name`）设成 SDK 的 app name，每个请求的 User-Agent 里带上它，服务端的访问日志里能看出是哪个应用发的。
    /// `presign_public_origin` 没有 scheme、带了路径也报 `AppErr::InvalidConfig`
    pub(super) async fn from_config(
        name: &str,
        config: &S3ClientConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        #[cfg(not(feature = "s3-bucket-admin"))]
        ensure!(
            !config.manage_buckets,
            AppErr::InvalidConfig,
            format!("s3 client {name:?}: manage_buckets needs hygiea feature aws-s3-bucket-admin")
        );
        if let Some(origin) = &config.presign_public_origin {
            check_presign_public_origin(name, origin)?;
        }
        let builder = if config.use_sdk_env {
            sdk_env_builder(name, config).await?
        } else {
            explicit_builder(name, config)?
        };
        let app_name = AppName::new(registry_config.app_name.clone()).map_err(|e| {
            impossible!(
                "registry.app_name 读配置时按 [a-z][a-z0-9_]* 检查过，都能当 SDK 的 app name：{:?} {e}",
                registry_config.app_name
            )
        })?;
        let sdk_config = config.apply_s3_options(builder.app_name(app_name)).build();
        Ok(Self {
            inner: Client::from_conf(sdk_config),
            presign_public_origin: config.presign_public_origin.clone(),
            name: name.to_owned(),
            region: config.region.clone(),
            manage_buckets: config.manage_buckets,
        })
    }

    /// 配置里的客户端名
    #[cfg_attr(not(feature = "s3-bucket-admin"), allow(dead_code))]
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// 能不能管理 bucket，见 [`S3ClientConfig::manage_buckets`]
    pub(super) fn manage_buckets(&self) -> bool {
        self.manage_buckets
    }

    /// 签预签名 URL 时这一次请求要覆盖的配置：配了 [`S3ClientConfig::presign_public_origin`] 的把服务地址换成它，签名就按它算；
    /// 没配的是空的，什么都不覆盖，按服务地址签
    pub(super) fn presign_config_override(&self) -> Builder {
        match &self.presign_public_origin {
            Some(origin) => Builder::new().endpoint_url(origin),
            None => Builder::new(),
        }
    }

    /// 建 bucket 时要带的 LocationConstraint，规则见 [`S3Region::location_constraint`]；没写 region 不带
    pub(super) fn location_constraint(&self) -> Option<BucketLocationConstraint> {
        self.region.as_ref().and_then(S3Region::location_constraint)
    }
}

impl Deref for AwsS3Client {
    type Target = Client;

    fn deref(&self) -> &Client {
        &self.inner
    }
}

/// `presign_public_origin` 只能有 scheme、host、端口（结尾多一个 `/` 也行）
fn check_presign_public_origin(name: &str, origin: &str) -> Result<()> {
    let Some((_, rest)) = origin.split_once("://") else {
        bail!(
            AppErr::InvalidConfig,
            format!("s3 client {name:?}: presign_public_origin has no scheme: {origin:?}")
        );
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    ensure!(
        !authority.is_empty() && !authority.contains(['/', '?', '#']),
        AppErr::InvalidConfig,
        format!(
            "s3 client {name:?}: presign_public_origin must be scheme://host[:port] without path: {origin:?}"
        )
    );
    Ok(())
}

/// 不用 SDK 环境：只用配置里写的，SDK 的其他设置（重试、超时等）是内置默认值
fn explicit_builder(name: &str, config: &S3ClientConfig) -> Result<Builder> {
    ensure!(
        config.profile.is_none(),
        AppErr::InvalidConfig,
        format!("s3 client {name:?}: profile needs use_sdk_env")
    );
    let region = config
        .region
        .as_ref()
        .ok_or_else(|| required_without_sdk_env(name, "region"))?;
    let credentials = config
        .credentials
        .as_ref()
        .ok_or_else(|| required_without_sdk_env(name, "credentials"))?;
    let mut builder = Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(region.as_str().to_owned()))
        .credentials_provider(credentials.to_sdk());
    if let Some(url) = &config.endpoint_url {
        builder = builder.endpoint_url(url);
    }
    Ok(builder)
}

fn required_without_sdk_env(name: &str, field: &str) -> HyErr {
    err!(
        AppErr::InvalidConfig,
        format!("s3 client {name:?}: {field} is required without use_sdk_env")
    )
}

/// 用 SDK 环境：没写的项由 SDK 从环境加载，写了的项优先
#[cfg(feature = "sdk-env")]
async fn sdk_env_builder(name: &str, config: &S3ClientConfig) -> Result<Builder> {
    let sdk_config = crate::config::load_sdk_config(
        config.region.as_ref().map(S3Region::as_str),
        config.endpoint_url.as_deref(),
        config.profile.as_deref(),
        config.credentials.as_ref(),
    )
    .await;
    tracing::info!(
        "S3 client config loaded from sdk env [client={name}, region={:?}]",
        sdk_config.region().map(|r| r.as_ref())
    );
    Ok(Builder::from(&sdk_config))
}

#[cfg(not(feature = "sdk-env"))]
async fn sdk_env_builder(name: &str, _config: &S3ClientConfig) -> Result<Builder> {
    bail!(
        AppErr::InvalidConfig,
        format!("s3 client {name:?}: use_sdk_env needs hygiea feature aws-sdk-env")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StaticCredentials;

    fn credentials() -> Option<StaticCredentials> {
        Some(StaticCredentials {
            access_key_id: "ak".into(),
            secret_access_key: "sk".into(),
            session_token: None,
        })
    }

    /// 不用 SDK 环境时：少了 region、credentials，写了 profile，都报 InvalidConfig
    #[tokio::test]
    async fn explicit_mode_requires_region_and_credentials() {
        let complete = S3ClientConfig {
            region: Some("auto".into()),
            credentials: credentials(),
            ..S3ClientConfig::default()
        };
        AwsS3Client::from_config("test", &complete, &RegistryConfig::default())
            .await
            .unwrap();
        for config in [
            S3ClientConfig {
                region: None,
                ..complete.clone()
            },
            S3ClientConfig {
                credentials: None,
                ..complete.clone()
            },
            S3ClientConfig {
                profile: Some("default".into()),
                ..complete.clone()
            },
        ] {
            let err = AwsS3Client::from_config("test", &config, &RegistryConfig::default())
                .await
                .unwrap_err();
            assert!(err.is(AppErr::InvalidConfig), "{config:?}: {err:#}");
        }
    }

    /// 没开 s3-bucket-admin feature 时设了 manage_buckets：报 InvalidConfig
    #[cfg(not(feature = "s3-bucket-admin"))]
    #[tokio::test]
    async fn manage_buckets_needs_feature() {
        let config = S3ClientConfig {
            region: Some("auto".into()),
            credentials: credentials(),
            manage_buckets: true,
            ..S3ClientConfig::default()
        };
        let err = AwsS3Client::from_config("test", &config, &RegistryConfig::default())
            .await
            .unwrap_err();
        assert!(err.is(AppErr::InvalidConfig), "{err:#}");
    }

    /// 没开 sdk-env feature 时设了 use_sdk_env：报 InvalidConfig
    #[cfg(not(feature = "sdk-env"))]
    #[tokio::test]
    async fn sdk_env_needs_feature() {
        let config = S3ClientConfig {
            use_sdk_env: true,
            ..S3ClientConfig::default()
        };
        let err = AwsS3Client::from_config("test", &config, &RegistryConfig::default())
            .await
            .unwrap_err();
        assert!(err.is(AppErr::InvalidConfig), "{err:#}");
    }

    /// 开了 sdk-env 时设了 use_sdk_env：没写 region、凭证也能建（由 SDK 环境提供，用到时才解析）
    #[cfg(feature = "sdk-env")]
    #[tokio::test]
    async fn sdk_env_allows_missing_fields() {
        let config = S3ClientConfig {
            use_sdk_env: true,
            ..S3ClientConfig::default()
        };
        AwsS3Client::from_config("test", &config, &RegistryConfig::default())
            .await
            .unwrap();
    }

    /// presign_public_origin 带路径、没有 scheme：报 InvalidConfig；结尾多一个 `/` 的可以
    #[tokio::test]
    async fn invalid_presign_public_origin_rejected() {
        let base = S3ClientConfig {
            region: Some("auto".into()),
            credentials: credentials(),
            ..S3ClientConfig::default()
        };
        for origin in ["https://x.com/s3", "x.com"] {
            let config = S3ClientConfig {
                presign_public_origin: Some(origin.into()),
                ..base.clone()
            };
            let err = AwsS3Client::from_config("test", &config, &RegistryConfig::default())
                .await
                .unwrap_err();
            assert!(err.is(AppErr::InvalidConfig), "{origin:?}: {err:#}");
        }
        let config = S3ClientConfig {
            presign_public_origin: Some("https://x.com:8443/".into()),
            ..base
        };
        AwsS3Client::from_config("test", &config, &RegistryConfig::default())
            .await
            .unwrap();
    }

    /// 预签名 URL：配了 presign_public_origin 的按它签（URL 的地址就是它），没配的按服务地址签
    #[tokio::test]
    async fn presigned_url_origin() {
        let base = S3ClientConfig {
            region: Some("auto".into()),
            credentials: credentials(),
            endpoint_url: Some("http://10.0.0.1:9000".into()),
            force_path_style: true,
            ..S3ClientConfig::default()
        };
        for (origin, expected) in [
            (None, "http://10.0.0.1:9000/b/k.mp3?"),
            (Some("https://x.com"), "https://x.com/b/k.mp3?"),
            (Some("https://x.com:8443/"), "https://x.com:8443/b/k.mp3?"),
        ] {
            let config = S3ClientConfig {
                presign_public_origin: origin.map(Into::into),
                ..base.clone()
            };
            let client = AwsS3Client::from_config("test", &config, &RegistryConfig::default())
                .await
                .unwrap();
            let request = client
                .get_object()
                .bucket("b")
                .key("k.mp3")
                .customize()
                .config_override(client.presign_config_override())
                .presigned(
                    aws_sdk_s3::presigning::PresigningConfig::expires_in(
                        std::time::Duration::from_secs(60),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            let url = request.uri();
            assert!(url.starts_with(expected), "{origin:?}: {url}");
            assert!(url.contains("X-Amz-SignedHeaders=host"), "{url}");
        }
    }

    /// 应用名设成 SDK 的 app name
    #[tokio::test]
    async fn app_name_goes_to_sdk() {
        let config = S3ClientConfig {
            region: Some("auto".into()),
            credentials: credentials(),
            ..S3ClientConfig::default()
        };
        let registry_config = RegistryConfig {
            app_name: "dict_jp".into(),
            ..RegistryConfig::default()
        };
        let client = AwsS3Client::from_config("test", &config, &registry_config)
            .await
            .unwrap();
        assert_eq!(
            client.config().app_name().map(|name| name.as_ref()),
            Some("dict_jp")
        );
    }
}
