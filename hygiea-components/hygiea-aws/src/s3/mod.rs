//! S3：组件 [`AwsS3Component`] 按 [`S3Config`] 建好默认的客户端和其他客户端、每个客户端下多个 bucket，放进 Resources 的是 [`AwsS3Store`]，
//! 从它按客户端和段名（[`S3BucketName`]：字符串，或者使用方自己的类型）取 bucket 句柄 [`S3Bucket`]；
//! `S3Bucket::with_prefix` 得到限定在一个前缀下的句柄 [`S3BucketWithPrefix`]。两种句柄的对象操作（上传、读、删、预签名 GET）
//! 都在 trait [`S3BucketHandle`] 里，要 `use` 它；按现成的 key 写入的 [`S3Bucket::put_object`] 只在 `S3Bucket` 上。
//! 上传的文件是 [`PutFile`]，key 按 [`PutKey`] 拼（上传前算出 [`PutDigest`]，调用方给出 [`PutDecision`]，结果是 [`PutOutcome`]）；
//! key 的前缀实现 [`S3KeyPrefix`]（字符串，或者使用方自己的类型）。读出来的是 [`ObjectStream`]、[`ObjectMeta`]，
//! 要内容的 sha256 的用 `get_with_sha256`、`head_with_sha256`；预签名 GET 的选项是 [`PresignGetOptions`]。
//! 错误是 [`S3Err`]。

mod bucket;
mod client;
mod config;
mod error;
mod store;

pub use bucket::{
    ObjectMeta, ObjectStream, PresignGetOptions, PutDecision, PutDigest, PutFile, PutKey,
    PutOutcome, S3Bucket, S3BucketHandle, S3BucketWithPrefix, S3KeyPrefix,
};
pub use config::{
    ChecksumWhen, DEFAULT_CLIENT_SECTION, S3BucketConfig, S3BucketsConfig, S3ClientConfig,
    S3Config, S3Region,
};
pub use error::S3Err;
pub use store::{AwsS3Component, AwsS3Store, S3BucketName};
