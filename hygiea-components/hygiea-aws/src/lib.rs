//! hygiea 的 AWS 组件：按 AWS 官方的分层来，一份公共配置，各服务在它上面叠加自己的配置。
//!
//! | feature | 服务 | 放进 Resources 的资源 |
//! |---|---|---|
//! | （常开） | 公共配置 | [`SdkConfig`] |
//! | `s3` | S3 | [`aws_sdk_s3::Client`] |
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
//! 同一个组件可以用不同名字注册多次（比如一个连 AWS、一个连 R2），各资源都按组件名放进 Resources。
//!
//! ```toml
//! hygiea-aws = { version = "..", features = ["s3"] }
//! ```

mod component;
mod config;
#[cfg(feature = "s3")]
mod s3;

pub use component::AwsComponent;
pub use config::{AwsConfig, StaticCredentials};
#[cfg(feature = "s3")]
pub use s3::{ChecksumWhen, S3Config};

// ---------------------------- 再导出 ----------------------------

/// aws-config 整个再导出，调用方不必自己依赖它（版本对不上时同名类型不兼容）
pub use aws_config;
pub use aws_config::SdkConfig;
/// S3 SDK 整个再导出，操作对象用 `aws_sdk_s3::Client` 上的方法
#[cfg(feature = "s3")]
pub use aws_sdk_s3;
