//! hygiea 的 AWS 组件：按 AWS 官方的分层来，一份公共配置，各服务在它上面叠加自己的配置。
//!
//! | feature | 服务 | 组件 | 放进 Resources 的资源 |
//! |---|---|---|---|
//! | `s3` | S3 | `AwsS3Component` | `AwsS3Client`（`Deref` 到 `aws_sdk_s3::Client`） |
//!
//! [`AwsConfig`] 对应 `aws_config::SdkConfig`：region、凭证、endpoint 等所有服务共用的项；
//! 每个服务一个子配置（[`AwsConfig::s3`]），对应 `aws_sdk_s3::config::Builder::from(&sdk_config)`
//! 之后再叠加的服务专属项。配置文件里写成：
//!
//! ```toml
//! [aws]
//! region = "auto"
//! endpoint_url = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
//! credentials = { access_key_id = "..", secret_access_key = ".." }
//!
//! [aws.s3]
//! force_path_style = false
//! ```
//!
//! 每个服务一个组件，配置都是 [`AwsConfig`]：服务资源的 `from_config` 借用同一份配置，取公共项和自己的子配置，
//! 以后加别的服务时可以共用一份。同一个组件可以用不同名字注册多次（比如一个连 AWS、一个连 R2），
//! 资源按组件名放进 Resources。
//!
//! ```toml
//! hygiea-aws = { version = "..", features = ["s3"] }
//! ```

mod config;
#[cfg(feature = "s3")]
mod s3;

pub use config::{AwsConfig, StaticCredentials};
#[cfg(feature = "s3")]
pub use s3::{AwsS3Client, AwsS3Component, ChecksumWhen, S3Config};

// ---------------------------- 再导出 ----------------------------

/// aws-config 整个再导出，调用方不必自己依赖它（版本对不上时同名类型不兼容）
pub use aws_config;
/// S3 SDK 整个再导出，请求、响应、错误类型从这里取
#[cfg(feature = "s3")]
pub use aws_sdk_s3;
