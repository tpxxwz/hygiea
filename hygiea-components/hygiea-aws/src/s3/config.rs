//! S3 组件的配置：[`S3Config`] 下多个客户端（[`S3ClientConfig`]），每个客户端下多个 bucket（[`S3BucketConfig`]）。

use std::collections::BTreeMap;
use std::time::Duration;

use aws_sdk_s3::config::{Builder, RequestChecksumCalculation, ResponseChecksumValidation};
use aws_sdk_s3::types::BucketLocationConstraint;
use serde::Deserialize;

use crate::StaticCredentials;

/// 默认客户端的段名：报错时当客户端名用，[`S3Config::extra`] 里不能用它。
/// 调用方要在日志、错误里指明是默认客户端时也用它
pub const DEFAULT_CLIENT_SECTION: &str = "default";

/// 默认 bucket 的段名：没写 `name` 时就是实际的 bucket 名，[`S3BucketsConfig::extra`] 里不能用它
pub(crate) const DEFAULT_BUCKET_SECTION: &str = "default";

/// 组件配置。默认的和其他客户端，每个客户端下默认的和其他 bucket，两层都是 `default` 必须写、`extra` 按名字。配置文件里写成：
///
/// ```toml
/// [s3.default]                          # 默认的客户端，必须写
/// region = "us-east-1"
/// endpoint_url = "http://127.0.0.1:9000"
/// force_path_style = true
///
/// [s3.default.buckets.default]          # 默认客户端的默认 bucket，必须写：store.default_client_default_bucket()
/// name = "myapp-files"                  # 服务上实际的 bucket 名，不写就是段名（这里是 default）
///
/// [s3.default.buckets.extra.images]     # 默认客户端的其他 bucket，按段名
/// name = "myapp-images"
///
/// [s3.extra.r2]                         # 其他客户端，一个名字一段，可以没有
/// region = "auto"
/// endpoint_url = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
///
/// [s3.extra.r2.buckets.default]         # 每个客户端都要有默认 bucket：store.client_default_bucket("r2")
/// name = "myapp-files"
/// ```
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    /// 默认的客户端，必须写（`[<段名>.default]`），没写加载配置时就报错
    pub default: S3ClientConfig,
    /// 其他客户端，key 是客户端名。不能是空的，也不能叫 `default`，不然启动报 `AppErr::InvalidConfig`
    #[serde(default)]
    pub extra: BTreeMap<String, S3ClientConfig>,
}

/// 一个客户端：连哪个服务、用什么凭证，以及这个服务上的 bucket。
///
/// 默认（`use_sdk_env = false`）只用这里写的，不读 SDK 自己的环境（`AWS_*` 环境变量、`~/.aws`、实例凭证），
/// 机器上给别的 AWS 程序用的设置影响不到它：`region`、`credentials` 必须写，`endpoint_url` 不写是 AWS 按 region 算出的官方地址，
/// 不能写 `profile`；重试、超时等用 SDK 内置的默认值。
/// `use_sdk_env = true`（要开 `sdk-env` feature）时没写的项由 SDK 从环境加载，写了的项优先
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3ClientConfig {
    /// 没写的项让 SDK 从环境加载（`AWS_REGION`、`AWS_ACCESS_KEY_ID`、`AWS_ENDPOINT_URL` 等环境变量、
    /// `~/.aws/config` 和 `~/.aws/credentials`、容器和 EC2 的实例凭证）。要开 `sdk-env` feature，没开时设了启动报错
    #[serde(default)]
    pub use_sdk_env: bool,
    /// 区域，见 [`S3Region`]
    pub region: Option<S3Region>,
    /// 服务地址，连兼容 S3 的第三方服务（R2、RustFS、MinIO）时填；连 AWS 不用填
    pub endpoint_url: Option<String>,
    /// 预签名 URL（给前端等外部直接访问）对外的地址，只能有 scheme、host、端口（如 `https://x.com`），带路径的启动报错。
    /// 预签名按它签名，URL 就是这个地址，其余请求照常连服务地址（`endpoint_url` 或 SDK 算出的）；不写就按服务地址签名。
    /// 用在服务端连内网地址、外部经别的地址访问同一个服务的时候，比如服务端连集群内的 RustFS、前端经公网域名访问：
    /// 签名覆盖 Host、路径、query，外部的请求要原样（Host 头、路径、query 都不改）转到服务，服务端收到的和签名时一样才能验签通过
    pub presign_public_origin: Option<String>,
    /// `~/.aws/config` 里的 profile 名，不写用 `AWS_PROFILE` 或 `default`。只在 `use_sdk_env` 时能用
    pub profile: Option<String>,
    /// 写死的 AccessKey / SecretKey。`use_sdk_env` 时写了就不从环境取凭证
    pub credentials: Option<StaticCredentials>,
    /// 用路径形式寻址（`https://host/bucket/key`），而不是把 bucket 放进域名
    /// （`https://bucket.host/key`）。RustFS、MinIO 这类自建服务一般要开，AWS 和 R2 不用
    #[serde(default)]
    pub force_path_style: bool,
    /// 什么时候给请求算校验和（CRC32 等）。不写用 SDK 默认的 `when_supported`；
    /// 不认新校验和请求头的第三方服务报错时改成 `when_required`
    pub request_checksum_calculation: Option<ChecksumWhen>,
    /// 什么时候校验响应的校验和。不写用 SDK 默认的 `when_supported`
    pub response_checksum_validation: Option<ChecksumWhen>,
    /// 这个客户端能不能管理 bucket：开了时启动检查发现 bucket 不存在就建，[`S3Bucket`](super::S3Bucket) 上删 bucket 的方法能用；
    /// 没开时 bucket 不存在启动报错，删 bucket 的方法报错。
    /// 要开 `s3-bucket-admin` feature，没开时设了启动报错；凭证也要有对应的权限（R2 是 Admin Read & Write，
    /// AWS、RustFS 是策略里允许 `s3:CreateBucket`、`s3:DeleteBucket` 等）
    #[serde(default)]
    pub manage_buckets: bool,
    /// 这个服务上的 bucket：默认的（必须写）和按段名的其他 bucket
    pub buckets: S3BucketsConfig,
}

/// 一个客户端下的 bucket，和客户端那一层一样：默认的必须写（`buckets.default`），没写加载配置时就报错；
/// 其他的按段名放在 `buckets.extra.<段名>`，段名不能是空的，也不能叫 `default`（启动报 `AppErr::InvalidConfig`）。
/// 默认的 bucket 段名就是 `default`
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3BucketsConfig {
    pub default: S3BucketConfig,
    /// key 是段名（见 [`S3BucketName`](super::S3BucketName)）
    #[serde(default)]
    pub extra: BTreeMap<String, S3BucketConfig>,
}

/// 预签名 URL 有效期的下限：SigV4 预签名最短 1 秒
pub(crate) const PRESIGN_MIN_EXPIRES_IN: Duration = Duration::from_secs(1);

/// 预签名 URL 有效期的上限：SigV4 预签名最长 7 天，也是 [`S3BucketConfig::presign_max_expires_in`] 不写时的值
pub(crate) const PRESIGN_MAX_EXPIRES_IN: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// 一个 bucket 的设置。段名：默认的是 `default`，其他的是它在 [`S3BucketsConfig::extra`] 里的 key
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct S3BucketConfig {
    /// 服务上实际的 bucket 名，发请求用它，不写就是段名。各环境的 bucket 名不一样时写。
    /// 不能是空的，不能和同一个客户端下别的段的段名一样；同一个客户端下各段的实际 bucket 名（写了的 `name`，没写的段名）不能重复。
    /// 不对的启动报 `AppErr::InvalidConfig`
    pub name: Option<String>,
    /// 这个 bucket 预签名 URL 的最长有效期，如 `"1h"`：签名时传的有效期不能超过它，不传就用它
    /// （见 [`S3BucketHandle::presigned_get_url`](super::S3BucketHandle::presigned_get_url)）。
    /// 不写是 7 天（SigV4 预签名的上限），要在 1 秒到 7 天之间，超出的启动报 `AppErr::InvalidConfig`
    #[serde(with = "hygiea_core::datetime::duration")]
    pub presign_max_expires_in: Duration,
}

impl Default for S3BucketConfig {
    fn default() -> Self {
        Self {
            name: None,
            presign_max_expires_in: PRESIGN_MAX_EXPIRES_IN,
        }
    }
}

impl S3BucketConfig {
    /// 服务上实际的 bucket 名：写了 `name` 用它，没写用段名 `section`
    pub(crate) fn bucket_name<'a>(&'a self, section: &'a str) -> &'a str {
        self.name.as_deref().unwrap_or(section)
    }
}

/// 区域，比如 `us-east-1`；Cloudflare R2 写 `auto`；RustFS、MinIO 随便写一个（一般是 `us-east-1`）。
/// 取值是开放的（AWS 会开新区域，自建服务随便写），配置里写字符串
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct S3Region(String);

impl S3Region {
    /// AWS 的默认区域，在这里建 bucket 时不能带 LocationConstraint
    pub const US_EAST_1: &str = "us-east-1";
    /// Cloudflare R2 的写法
    pub const AUTO: &str = "auto";

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 建 bucket 时要带的 LocationConstraint：`us-east-1`、`auto` 不带，其他区域带
    pub(crate) fn location_constraint(&self) -> Option<BucketLocationConstraint> {
        match self.0.as_str() {
            Self::US_EAST_1 | Self::AUTO => None,
            other => Some(BucketLocationConstraint::from(other)),
        }
    }
}

impl From<&str> for S3Region {
    fn from(region: &str) -> Self {
        Self(region.to_owned())
    }
}

impl From<String> for S3Region {
    fn from(region: String) -> Self {
        Self(region)
    }
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

impl S3ClientConfig {
    /// 在 SDK 的 config builder 上叠加 S3 专属项（寻址方式、校验和）
    pub(crate) fn apply_s3_options(&self, mut builder: Builder) -> Builder {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置模板 `config/aws.toml` 就是代码里的默认值：按配置结构体解析（deny_unknown_fields，写错、多写的项解析失败），
    /// 默认客户端的各项都是 [`S3ClientConfig`] 的默认值、没有其他客户端。结构体没有实现 PartialEq，比 Debug 输出
    #[test]
    fn template_is_code_defaults() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Template {
            s3: S3Config,
        }
        let template: Template = toml::from_str(include_str!("../../config/aws.toml")).unwrap();
        let defaults = S3Config {
            default: S3ClientConfig::default(),
            extra: BTreeMap::new(),
        };
        assert_eq!(format!("{:?}", template.s3), format!("{defaults:?}"));
    }

    /// 没写默认的客户端、客户端没写默认的 bucket：解析时报错
    #[test]
    fn defaults_required() {
        assert!(toml::from_str::<S3Config>("").is_err());
        assert!(toml::from_str::<S3Config>("[extra.r2]\n[extra.r2.buckets.default]").is_err());
        assert!(toml::from_str::<S3Config>("[default]").is_err());
        assert!(toml::from_str::<S3Config>("[default.buckets.extra.files]").is_err());
        assert!(toml::from_str::<S3Config>("[default.buckets.default]\n[extra.r2]").is_err());
        toml::from_str::<S3Config>("[default.buckets.default]").unwrap();
    }

    /// 配置文件写法：默认的客户端、其他客户端，下面嵌套 bucket
    #[test]
    fn deserializes_nested_buckets() {
        let config: S3Config = toml::from_str(
            r#"
            [default]
            force_path_style = true
            request_checksum_calculation = "when_required"

            [default.buckets.default]

            [default.buckets.extra.my-bucket]

            [extra.r2]
            region = "auto"
            endpoint_url = "https://example.r2.cloudflarestorage.com"
            credentials = { access_key_id = "ak", secret_access_key = "sk" }

            [extra.r2.buckets.default]
            name = "app-files"
            presign_max_expires_in = "1h"
            "#,
        )
        .unwrap();
        let r2 = &config.extra["r2"];
        assert_eq!(r2.region.as_ref().map(S3Region::as_str), Some("auto"));
        assert_eq!(
            r2.buckets.default.bucket_name(DEFAULT_BUCKET_SECTION),
            "app-files"
        );
        assert_eq!(
            r2.buckets.default.presign_max_expires_in,
            Duration::from_secs(3600)
        );
        assert!(r2.buckets.extra.is_empty());
        let rustfs = &config.default;
        assert!(rustfs.force_path_style);
        assert_eq!(
            rustfs.request_checksum_calculation,
            Some(ChecksumWhen::WhenRequired)
        );
        assert_eq!(
            rustfs.buckets.default.bucket_name(DEFAULT_BUCKET_SECTION),
            "default"
        );
        assert_eq!(
            rustfs.buckets.extra["my-bucket"].bucket_name("my-bucket"),
            "my-bucket"
        );
        assert_eq!(
            rustfs.buckets.extra["my-bucket"].presign_max_expires_in,
            PRESIGN_MAX_EXPIRES_IN
        );
    }

    /// 客户端、同一个客户端下的 bucket 写重了：解析时报错
    #[test]
    fn duplicate_tables_rejected() {
        assert!(
            toml::from_str::<S3Config>(
                "[default.buckets.default]\n[extra.a.buckets.default]\n[extra.a.buckets.default]"
            )
            .is_err()
        );
        assert!(
            toml::from_str::<S3Config>(
                "[default.buckets.default]\n[default.buckets.extra.x]\n[default.buckets.extra.x]"
            )
            .is_err()
        );
    }

    /// us-east-1、auto 不带 LocationConstraint，其他区域带
    #[test]
    fn location_constraint_by_region() {
        assert_eq!(S3Region::from("us-east-1").location_constraint(), None);
        assert_eq!(S3Region::from("auto").location_constraint(), None);
        assert_eq!(
            S3Region::from("ap-northeast-1").location_constraint(),
            Some(BucketLocationConstraint::ApNortheast1)
        );
    }

    /// 拼错的项启动时报错，不被静默忽略
    #[test]
    fn rejects_unknown_fields() {
        assert!(toml::from_str::<S3ClientConfig>(r#"regoin = "x""#).is_err());
        assert!(toml::from_str::<S3BucketConfig>("prefix = \"x\"").is_err());
    }

    /// 没写的项不动 builder
    #[test]
    fn unset_options_keep_builder() {
        let conf = S3ClientConfig::default()
            .apply_s3_options(Builder::new())
            .build();
        assert_eq!(conf.request_checksum_calculation(), None);
    }

    /// 写了的项叠加上去
    #[test]
    fn overrides_apply() {
        let config = S3ClientConfig {
            force_path_style: true,
            request_checksum_calculation: Some(ChecksumWhen::WhenRequired),
            response_checksum_validation: Some(ChecksumWhen::WhenRequired),
            ..S3ClientConfig::default()
        };
        let conf = config.apply_s3_options(Builder::new()).build();
        assert_eq!(
            conf.request_checksum_calculation(),
            Some(&RequestChecksumCalculation::WhenRequired)
        );
        assert_eq!(
            conf.response_checksum_validation(),
            Some(&ResponseChecksumValidation::WhenRequired)
        );
    }
}
