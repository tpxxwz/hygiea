//! S3 的错误。和 [`BaseErr`](hygiea_core::BaseErr) 共用项目前缀 999，内部模块前缀是 102，错误码全局唯一

use std::error::Error;
use std::time::Duration;

use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use hygiea_core::{HyErr, err, hy_err};

/// S3 各阶段的错误，错误码按阶段分段：启动 `0x`、发出之前 `1x`、发出之后 `2x`。
/// 只看配置值就能判断的错误（配置里的地址写错、bucket 的 name 重复等）报 `AppErr::InvalidConfig`，不在这里；
/// 调用方传的参数不合规则（扩展名、key 前缀、key 的名字、预签名有效期）报这里的变体：
///
/// - 启动和取客户端、bucket：[`BucketNotFound`](Self::BucketNotFound)、[`BucketNotConfigured`](Self::BucketNotConfigured)、
///   [`BucketCreateFailed`](Self::BucketCreateFailed)、[`ClientNotConfigured`](Self::ClientNotConfigured)
/// - 发出之前：[`ReadFileFailed`](Self::ReadFileFailed)、[`BucketAdminDisabled`](Self::BucketAdminDisabled)、
///   [`ContentTypeMismatch`](Self::ContentTypeMismatch)、[`InvalidFileExt`](Self::InvalidFileExt)、
///   [`InvalidKey`](Self::InvalidKey)、[`InvalidPresignExpiry`](Self::InvalidPresignExpiry)
/// - 发出之后：[`RequestFailed`](Self::RequestFailed)、[`ServiceError`](Self::ServiceError)、[`InvalidResponse`](Self::InvalidResponse)、
///   [`DeleteObjectsFailed`](Self::DeleteObjectsFailed)。前三个的 err_args 统一带 `op`（操作名，如 `put_object`）、
///   `bucket`（实际的 bucket 名）、`key`（完整 key，bucket 级的操作是空串）
///
/// 原始错误挂在 source 上，要看 SDK 的错误细节时 downcast 成 `aws_sdk_s3::error::SdkError<..>`
#[derive(hy_err)]
#[err_code_internal_module_prefix = "102"]
pub enum S3Err {
    // ---- 启动和取客户端、bucket ----
    /// 启动时 bucket 不存在，所在的客户端没开 `manage_buckets`（不让建）
    #[error(
        err_code = "01",
        err_tpl = "S3 bucket not found: {{ client }} {{ bucket }}"
    )]
    BucketNotFound,
    /// 按段名取客户端下的其他 bucket 时（[`AwsS3Store::client_buckets`](super::AwsS3Store::client_buckets) 等），
    /// 这个客户端下没配这个 bucket
    #[error(
        err_code = "02",
        err_tpl = "S3 bucket not configured: {{ client }} {{ bucket }}"
    )]
    BucketNotConfigured,
    /// 启动时 bucket 不存在，客户端开了 `manage_buckets`，但建 bucket 失败（比如凭证没有建 bucket 的权限）。
    /// `status`、`code` 是服务端返回的 HTTP 状态和 S3 错误码，没拿到服务端响应时是 `null`；原始错误挂在 source 上
    #[error(
        err_code = "03",
        err_tpl = "S3 bucket create failed: {{ client }} {{ bucket }}"
    )]
    BucketCreateFailed,
    /// 按名字取其他客户端时（[`AwsS3Store::client_default_bucket`](super::AwsS3Store::client_default_bucket) 等）配置里没有这个客户端
    #[error(err_code = "04", err_tpl = "S3 client not configured: {{ client }}")]
    ClientNotConfigured,

    // ---- 发出之前 ----
    /// 上传的本地文件打不开、读不了，原始 io 错误挂在 source 上
    #[error(err_code = "11", err_tpl = "S3 read file failed: {{ path }}")]
    ReadFileFailed,
    /// 调删 bucket 这类管理 bucket 的方法，但所在的客户端没开 `manage_buckets`
    #[error(
        err_code = "12",
        err_tpl = "S3 bucket admin disabled: {{ client }} {{ bucket }}"
    )]
    BucketAdminDisabled,
    /// [`S3BucketHandle::put`](super::S3BucketHandle::put) 时按内容开头的字节认出的类型和给的扩展名对不上，比如 PNG 文件给的是 `mp3`。
    /// `file_ext` 是给的扩展名（转过小写），`detected` 是认出的 MIME 类型
    #[error(
        err_code = "13",
        err_tpl = "S3 content type mismatch: .{{ file_ext }} is {{ detected }}"
    )]
    ContentTypeMismatch,
    /// [`S3BucketHandle::put`](super::S3BucketHandle::put) 给的扩展名不能用：空的，或者有字母数字以外的字符（包括开头的 `.`）。
    /// `file_ext` 是给的扩展名
    #[error(err_code = "14", err_tpl = "S3 invalid file ext: {{ file_ext }}")]
    InvalidFileExt,
    /// key 的一段不合规则（空的、以 `/` 开头、有 `//`）：[`S3BucketHandle::put`](super::S3BucketHandle::put) 的 `key_prefix`、
    /// [`S3Bucket::with_prefix`](super::S3Bucket::with_prefix) 的前缀（见 [`S3KeyPrefix`](super::S3KeyPrefix)），
    /// [`PutKey::Named`](super::PutKey::Named) 的名字。`part` 是哪一段（`prefix` / `name`），`value` 是给的值，`reason` 是哪条规则没过
    #[error(
        err_code = "15",
        err_tpl = "S3 invalid key {{ part }}: {{ value }} ({{ reason }})"
    )]
    InvalidKey,
    /// 预签名传的有效期（[`S3BucketHandle::presigned_get_url`](super::S3BucketHandle::presigned_get_url) 的 `expires_in`）
    /// 不到 1 秒，或者超过了 bucket 配的最长有效期
    /// （[`S3BucketConfig::presign_max_expires_in`](super::S3BucketConfig::presign_max_expires_in)，`max`）
    #[error(
        err_code = "16",
        err_tpl = "S3 invalid presign expiry: {{ expires_in }} (max {{ max }}) for {{ bucket }} {{ key }}"
    )]
    InvalidPresignExpiry,

    // ---- 发出之后 ----
    /// 没拿到服务端的错误响应：超时、连不上、TLS 失败、读 body 中断、响应解析不了
    #[error(
        err_code = "21",
        err_tpl = "S3 request failed: {{ op }} {{ bucket }} {{ key }}"
    )]
    RequestFailed,
    /// 服务端返回了错误：`status` 是 HTTP 状态，`code` 是 S3 的错误码（AccessDenied、NoSuchBucket 等，可能没有）。
    /// 对象不存在（404）的 get、head 不报这个，返回 `None`
    #[error(
        err_code = "22",
        err_tpl = "S3 {{ status }} {{ code }}: {{ op }} {{ bucket }} {{ key }}"
    )]
    ServiceError,
    /// 服务端返回成功，但响应里缺了必需的东西，比如列出的对象没有 key、还有下一页却没给翻页标记。`field` 是缺的东西
    #[error(
        err_code = "23",
        err_tpl = "S3 invalid response: {{ op }} {{ bucket }} missing {{ field }}"
    )]
    InvalidResponse,
    /// 批量删对象（DeleteObjects）返回成功，但有对象没删掉。`failed` 是没删掉的个数，`key`、`code` 是第一个的 key 和 S3 错误码
    #[error(
        err_code = "24",
        err_tpl = "S3 delete objects failed: {{ bucket }} {{ failed }} failed, first {{ key }} {{ code }}"
    )]
    DeleteObjectsFailed,
    /// [`get_with_sha256`](super::S3BucketHandle::get_with_sha256)、[`head_with_sha256`](super::S3BucketHandle::head_with_sha256)
    /// 时对象上没有 sha256：上传时没带，比如不经 hygiea 传的
    #[error(
        err_code = "25",
        err_tpl = "S3 object missing sha256: {{ bucket }} {{ key }}"
    )]
    MissingSha256,
}

// ---------------- 构造函数 ----------------
//
// 每个变体一个，模块内部统一从这里构造：错误里带哪些参数、原始错误怎么挂到 source 上，都只在这里写一次

pub(super) fn bucket_not_found(client: &str, bucket: &str) -> HyErr {
    err!(S3Err::BucketNotFound, { "client": client, "bucket": bucket })
}

pub(super) fn bucket_not_configured(client: &str, bucket: &str) -> HyErr {
    err!(S3Err::BucketNotConfigured, { "client": client, "bucket": bucket })
}

pub(super) fn client_not_configured(client: &str) -> HyErr {
    err!(S3Err::ClientNotConfigured, { "client": client })
}

pub(super) fn bucket_create_failed<E>(
    client: &str,
    bucket: &str,
    e: SdkError<E, HttpResponse>,
) -> HyErr
where
    E: ProvideErrorMetadata + Error + Send + Sync + 'static,
{
    let (status, code) = match &e {
        SdkError::ServiceError(service_error) => (
            Some(service_error.raw().status().as_u16()),
            service_error.err().code().map(str::to_owned),
        ),
        _ => (None, None),
    };
    err!(S3Err::BucketCreateFailed, {
        "client": client,
        "bucket": bucket,
        "status": status,
        "code": code,
    })
    .with_source(e)
}

// 只有管理 bucket 的方法用
#[cfg_attr(not(feature = "s3-bucket-admin"), allow(dead_code))]
pub(super) fn bucket_admin_disabled(client: &str, bucket: &str) -> HyErr {
    err!(S3Err::BucketAdminDisabled, { "client": client, "bucket": bucket })
}

#[cfg_attr(not(feature = "s3-bucket-admin"), allow(dead_code))]
pub(super) fn invalid_response(op: &str, bucket: &str, field: &str) -> HyErr {
    err!(S3Err::InvalidResponse, { "op": op, "bucket": bucket, "key": "", "field": field })
}

#[cfg_attr(not(feature = "s3-bucket-admin"), allow(dead_code))]
pub(super) fn delete_objects_failed(
    bucket: &str,
    failed: usize,
    key: Option<&str>,
    code: Option<&str>,
) -> HyErr {
    err!(S3Err::DeleteObjectsFailed, {
        "bucket": bucket,
        "failed": failed,
        "key": key,
        "code": code,
    })
}

pub(super) fn missing_sha256(bucket: &str, key: &str) -> HyErr {
    err!(S3Err::MissingSha256, { "bucket": bucket, "key": key })
}

pub(super) fn content_type_mismatch(file_ext: &str, detected: &str) -> HyErr {
    err!(S3Err::ContentTypeMismatch, { "file_ext": file_ext, "detected": detected })
}

pub(super) fn invalid_file_ext(file_ext: &str) -> HyErr {
    err!(S3Err::InvalidFileExt, { "file_ext": file_ext })
}

pub(super) fn invalid_key(part: &str, value: &str, reason: &str) -> HyErr {
    err!(S3Err::InvalidKey, { "part": part, "value": value, "reason": reason })
}

pub(super) fn invalid_presign_expiry(
    expires_in: Duration,
    max: Duration,
    bucket: &str,
    key: &str,
) -> HyErr {
    err!(S3Err::InvalidPresignExpiry, {
        "expires_in": format!("{expires_in:?}"),
        "max": format!("{max:?}"),
        "bucket": bucket,
        "key": key,
    })
}

pub(super) fn read_file_failed(path: &str, e: impl Error + Send + Sync + 'static) -> HyErr {
    err!(S3Err::ReadFileFailed, path).with_source(e)
}

/// 读 body 中断这类拿不到 [`SdkError`] 的请求失败
pub(super) fn request_failed(
    op: &str,
    bucket: &str,
    key: &str,
    e: impl Error + Send + Sync + 'static,
) -> HyErr {
    err!(S3Err::RequestFailed, { "op": op, "bucket": bucket, "key": key }).with_source(e)
}

/// SDK 调用失败：服务端返回了错误是 [`S3Err::ServiceError`]，其他是 [`S3Err::RequestFailed`]
pub(super) fn sdk_error<E>(op: &str, bucket: &str, key: &str, e: SdkError<E, HttpResponse>) -> HyErr
where
    E: ProvideErrorMetadata + Error + Send + Sync + 'static,
{
    let SdkError::ServiceError(service_error) = &e else {
        return request_failed(op, bucket, key, e);
    };
    let status = service_error.raw().status().as_u16();
    let code = service_error.err().code().map(str::to_owned);
    err!(S3Err::ServiceError, {
        "status": status,
        "code": code,
        "op": op,
        "bucket": bucket,
        "key": key,
    })
    .with_source(e)
}
