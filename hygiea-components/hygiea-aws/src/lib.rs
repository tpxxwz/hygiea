//! hygiea 的 AWS 组件：每个服务一个组件，按配置建好多个客户端，放进 Resources 后按名字取。
//!
//! | feature | 服务 | 组件 | 放进 Resources 的资源 |
//! |---|---|---|---|
//! | `s3` | S3 | `AwsS3Component` | `AwsS3Store`：默认的客户端和其他客户端，每个客户端下多个 bucket（`S3Bucket`，对象操作在 `S3BucketHandle` 里） |
//!
//! 用到才开的 feature（facade 里是前面加 `aws-`）：
//!
//! | feature | 打开什么 |
//! |---|---|
//! | `sdk-env` | 客户端可以设 `use_sdk_env = true`：没写的项由 SDK 从环境加载（`AWS_*` 环境变量、`~/.aws`、实例凭证）。引入 `aws-config` |
//! | `sso` | SDK 环境里的 SSO profile（`~/.aws/config` 里的 `sso_*`）；会带上 `sdk-env` |
//! | `credentials-process` | SDK 环境里的 `credential_process`（跑外部程序取凭证）；会带上 `sdk-env` |
//! | `s3-sigv4a` | S3 的 SigV4a 签名，AWS 的多区域访问点要用；会带上 `s3` |
//! | `s3-bucket-admin` | 管理 bucket：客户端可以设 `manage_buckets = true`（启动时建不存在的 bucket），`S3Bucket` 上有删 bucket 的方法；会带上 `s3` |
//!
//! 客户端默认只用配置里写的（`region`、`credentials` 必须写），不读 SDK 自己的环境，机器上给别的 AWS 程序用的设置影响不到它，
//! 连 R2、RustFS 这类服务用这种；连 AWS、想用 `~/.aws` 或实例凭证的客户端开 `sdk-env` 再设 `use_sdk_env = true`，见 [`S3ClientConfig`]。
//! 没开 `sso`、`credentials-process` 时 profile 里写了它们，启动不报错，第一次调用服务取凭证时报错。
//!
//! S3 的配置文件里写成（客户端和 bucket 两层都是默认的必须有、其他的按名字）：
//!
//! ```toml
//! [s3.default]                                   # 默认的客户端
//! region = "us-east-1"
//! endpoint_url = "http://127.0.0.1:9000"
//! credentials = { access_key_id = "..", secret_access_key = ".." }   # 用环境变量给，见下
//! force_path_style = true
//! manage_buckets = true                          # 启动时 bucket 不存在就建（要开 s3-bucket-admin）
//!
//! [s3.default.buckets.default]                   # 默认客户端的默认 bucket：store.default_client_default_bucket()
//! name = "myapp-files"                           # 服务上实际的 bucket 名，不写就是段名（default）
//! presign_max_expires_in = "1h"                  # 预签名 URL 的最长有效期，签名时不传就用它，不写是 7 天
//!
//! [s3.default.buckets.extra.images]              # 其他 bucket，按段名
//! name = "myapp-images"
//!
//! [s3.extra.r2]                                  # 其他客户端，不能叫 default
//! region = "auto"
//! endpoint_url = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
//! credentials = { access_key_id = "..", secret_access_key = ".." }
//!
//! [s3.extra.r2.buckets.default]                  # 每个客户端都要有默认 bucket：store.client_default_bucket("r2")
//! name = "myapp-files"
//! ```
//!
//! 凭证不要写进提交的文件，用环境变量按名字给，比如 `<前缀>__S3__EXTRA__R2__CREDENTIALS__ACCESS_KEY_ID`。
//! 完整的配置项见 `config/aws.toml`。
//!
//! ```toml
//! hygiea-aws = { version = "..", features = ["s3"] }
//! ```

mod config;
#[cfg(feature = "s3")]
mod s3;

pub use config::StaticCredentials;
#[cfg(feature = "s3")]
pub use s3::{
    AwsS3Component, AwsS3Store, ChecksumWhen, ObjectMeta, ObjectStream, PresignGetOptions,
    PutDecision, PutDigest, PutFile, PutKey, PutOutcome, S3Bucket, S3BucketConfig, S3BucketHandle,
    S3BucketName, S3BucketWithPrefix, S3BucketsConfig, S3ClientConfig, S3Config, S3Err,
    S3KeyPrefix, S3Region,
};

// ---------------------------- 再导出 ----------------------------

/// aws-config 整个再导出，调用方不必自己依赖它（版本对不上时同名类型不兼容）
#[cfg(feature = "sdk-env")]
pub use aws_config;
/// S3 SDK 整个再导出，请求、响应、错误类型从这里取
#[cfg(feature = "s3")]
pub use aws_sdk_s3;
/// [`PutFile::Stream`]、[`ObjectStream::body`] 的字节流（S3 SDK 的类型）
#[cfg(feature = "s3")]
pub use aws_sdk_s3::primitives::ByteStream;
/// [`PutFile`]、[`S3Bucket::put_object`] 的内容、[`ObjectStream::bytes`] 的结果
#[cfg(feature = "s3")]
pub use bytes::Bytes;
/// [`AwsS3Store::clients_default_buckets`] 等的参数和结果（开了 `serde`，能直接写在配置里）
#[cfg(feature = "s3")]
pub use indexmap::{IndexMap, IndexSet};
