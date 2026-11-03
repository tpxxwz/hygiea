//! [`S3Bucket`]：一个 bucket 的句柄（客户端 + bucket 名），key 原样发出；[`S3BucketWithPrefix`]：限定在一个前缀下的句柄，
//! key 都加上这个前缀。两者共有的对象操作（上传、读、删、预签名 GET）在 [`S3BucketHandle`] 里；
//! 按现成的 key 写入（[`S3Bucket::put_object`]）、删 bucket 只在 `S3Bucket` 上。
//! 上传的内容是 [`PutFile`]，key 按 [`PutKey`] 拼（上传前算出 [`PutDigest`]，调用方给出 [`PutDecision`]，结果是 [`PutOutcome`]），
//! key 的前缀实现 [`S3KeyPrefix`]。读出来的是 [`ObjectStream`]、[`ObjectMeta`]；预签名 GET 的选项是 [`PresignGetOptions`]。

use std::path::PathBuf;
use std::time::Duration;

use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::ChecksumMode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
#[cfg(feature = "s3-bucket-admin")]
use hygiea_core::bail;
use hygiea_core::{Result, ensure, impossible};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use super::client::AwsS3Client;
use super::config::PRESIGN_MIN_EXPIRES_IN;
#[cfg(feature = "s3-bucket-admin")]
use super::error::{bucket_admin_disabled, delete_objects_failed, invalid_response};
use super::error::{
    content_type_mismatch, invalid_file_ext, invalid_key, invalid_presign_expiry, missing_sha256,
    read_file_failed, request_failed, sdk_error,
};

// ---------------- 句柄 ----------------

/// 一个 bucket 的句柄，key 原样发出。从 [`AwsS3Store`](super::AwsS3Store) 取，clone 很便宜，不发请求。
/// 对象操作见 [`S3BucketHandle`]
#[derive(Clone, Debug)]
pub struct S3Bucket {
    client: AwsS3Client,
    bucket: String,
    /// 预签名 URL 的最长有效期，见 [`S3BucketConfig::presign_max_expires_in`](super::S3BucketConfig::presign_max_expires_in)
    presign_max_expires_in: Duration,
}

/// 限定在 `prefix` 下的 bucket 句柄（[`S3Bucket::with_prefix`] 得到）：对象操作（见 [`S3BucketHandle`]）收的 key 都相对 `prefix`，
/// 发请求时加上它，`put` 返回的 key 也是相对它的。clone 很便宜，不发请求。
/// 不能再套一层前缀，也没有删 bucket 的方法（删的是整个 bucket，不只是前缀下的）和 [`S3Bucket::put_object`]（见它的说明）
#[derive(Clone, Debug)]
pub struct S3BucketWithPrefix {
    bucket: S3Bucket,
    /// 检查、规整过的前缀，以 `/` 结尾
    prefix: String,
}

impl S3Bucket {
    pub(super) fn new(
        client: AwsS3Client,
        bucket: String,
        presign_max_expires_in: Duration,
    ) -> Self {
        Self {
            client,
            bucket,
            presign_max_expires_in,
        }
    }

    /// 限定在 `prefix` 下的句柄，见 [`S3BucketWithPrefix`]。前缀不符合 [`S3KeyPrefix`] 的规则报 [`S3Err::InvalidKey`](super::S3Err::InvalidKey)
    pub fn with_prefix(&self, prefix: impl S3KeyPrefix) -> Result<S3BucketWithPrefix> {
        Ok(S3BucketWithPrefix {
            bucket: self.clone(),
            prefix: normalized_key_prefix(prefix)?,
        })
    }
}

impl sealed::Sealed for S3Bucket {
    fn s3_bucket(&self) -> &S3Bucket {
        self
    }

    fn request_key(&self, key: &str) -> String {
        key.to_owned()
    }
}

impl S3BucketHandle for S3Bucket {}

impl sealed::Sealed for S3BucketWithPrefix {
    fn s3_bucket(&self) -> &S3Bucket {
        &self.bucket
    }

    fn request_key(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }
}

impl S3BucketHandle for S3BucketWithPrefix {}

// ---------------- 对象操作 ----------------

/// 句柄内部用的：底下的 bucket、按句柄的 key 拼出发请求用的完整 key。在私有模块里，外面实现不了 [`S3BucketHandle`]，也调不到这些方法
mod sealed {
    use super::S3Bucket;

    pub trait Sealed {
        fn s3_bucket(&self) -> &S3Bucket;

        /// 发请求用的完整 key：`S3Bucket` 原样，`S3BucketWithPrefix` 加上前缀
        fn request_key(&self, key: &str) -> String;
    }
}

/// bucket 句柄的对象操作，[`S3Bucket`] 和 [`S3BucketWithPrefix`] 都有。
/// 方法收的 key 都是相对句柄的 key（`S3Bucket` 原样发出，`S3BucketWithPrefix` 加上它的前缀）；
/// 错误里的 `key` 是发请求用的完整 key。要 `use` 这个 trait 才能调用这些方法，只能由 hygiea 实现。
///
/// 方法返回的 future 在具体的句柄类型上调用时是 `Send` 的（只要传进去的闭包也是）；
/// 在泛型代码里（`T: S3BucketHandle`）推不出 `Send`，要放进 `tokio::spawn` 这类要求 `Send` 的地方时用具体类型
#[allow(async_fn_in_trait)]
pub trait S3BucketHandle: sealed::Sealed + Sync {
    /// 上传：先读一遍内容，定下 [`PutDigest`]（sha256、字节数、Content-Type、扩展名），交给 `decide` 决定不传
    /// （[`PutDecision::Skip`]）还是传（[`PutDecision::Put`]）；`decide` 报错就不传，原样返回它的错误。
    ///
    /// Content-Type 和扩展名按内容开头的字节（文件头）定，[`PutFile`] 带的扩展名（不带 `.`，如 `mp3`，可以没有）作参考：
    /// - 文件头认得出来的：用认出的类型和它的扩展名。带了扩展名的要和认出的对得上（扩展名一样，或者扩展名对应的类型里有它，不分大小写），
    ///   对不上报 [`S3Err::ContentTypeMismatch`](super::S3Err::ContentTypeMismatch)；对得上的也用认出的扩展名（给 `jpeg` 认出 `jpg` 的用 `jpg`），
    ///   同样的内容扩展名总是一样
    /// - 认不出来的（纯文本这类没有固定文件头的）：扩展名用带的（转成小写），类型按它推断；推断不出、没带的是 `application/octet-stream`，
    ///   没带的没有扩展名
    ///
    /// 要传的 key（相对句柄）是 `key_prefix` + 主文件名 + `.` + 扩展名，没有扩展名的不带 `.`；主文件名按 `key` 定（见 [`PutKey`]）：
    /// [`PutKey::Content`] 是 sha256（[`PutDigest::sha256_hex`]），内容、前缀、扩展名一样 key 就一样，上面已有的对象被覆盖（内容也一样）；
    /// [`PutKey::Named`] 是给的名字，名字一样就覆盖，内容可能不一样。扩展名的定法两种一样。
    ///
    /// 在读内容之前检查：带的扩展名不能是空的、只能是字母和数字，不对的报 [`S3Err::InvalidFileExt`](super::S3Err::InvalidFileExt)；
    /// `key_prefix` 不符合 [`S3KeyPrefix`] 的规则、[`PutKey::Named`] 的名字不合规则报 [`S3Err::InvalidKey`](super::S3Err::InvalidKey)。
    ///
    /// [`PutFile::Stream`] 不读内容：sha256、字节数、Content-Type、扩展名用它带的（扩展名照样检查、转小写，不和类型核对），
    /// 跳过的话流不会被读。
    ///
    /// 一次 PutObject 上传（最大 5GB，本地文件、流边读边传），Content-Type 是定下来的类型，带上 sha256（`x-amz-checksum-sha256`）
    /// 和字节数（`Content-Length`）：服务端收到的内容和它们对不上就拒绝写入（报 [`S3Err::ServiceError`](super::S3Err::ServiceError)，
    /// 比如本地文件在算完之后被改了、流带的 sha256 不对）。流只能读一遍，上传中途失败 SDK 不重试，直接报错。
    /// 不写 Content-Disposition，下载时要指定文件名的在签名时给（[`PresignGetOptions::response_content_disposition`]）。
    /// 读不了本地文件报 [`S3Err::ReadFileFailed`](super::S3Err::ReadFileFailed)
    async fn put<T>(
        &self,
        key_prefix: impl S3KeyPrefix,
        key: PutKey,
        file: PutFile,
        decide: impl AsyncFnOnce(&PutDigest) -> Result<PutDecision<T>>,
    ) -> Result<PutOutcome<T>> {
        let key_prefix = normalized_key_prefix(key_prefix)?;
        key.check()?;
        let digest = file.digest().await?;
        if let PutDecision::Skip(value) = decide(&digest).await? {
            return Ok(PutOutcome::Skipped(value));
        }
        let key = key.into_key(&key_prefix, &digest);
        self.s3_bucket()
            .put_object(
                &self.request_key(&key),
                file.into_byte_stream().await?,
                &digest.sha256_base64(),
                digest.size,
                &digest.content_type,
            )
            .await?;
        Ok(PutOutcome::Put { key, digest })
    }

    /// 元数据和内容的流（[`ObjectStream`]），不读进内存，怎么读由调用方决定：直接用 [`body`](ObjectStream::body)
    /// 边读边处理（写进 HTTP 响应、文件，或者交给 [`S3Bucket::put_object`] 转到别处），要整段内容的用
    /// [`ObjectStream::bytes`]。对象不存在（NoSuchKey）返回 `None`；bucket 不存在等其他错误照常报错。
    /// 不向服务端要内容的 sha256，要的用 [`get_with_sha256`](Self::get_with_sha256)
    async fn get(&self, key: &str) -> Result<Option<ObjectStream>> {
        let object = get_object(self.s3_bucket(), self.request_key(key), false).await?;
        Ok(object.map(|(object, _)| object))
    }

    /// 同 [`get`](Self::get)，同时向服务端要内容的 sha256 的 base64（[`PutDigest::sha256_base64`]，SDK 读内容时会顺带按它校验）。
    /// [`put`](Self::put)、[`S3Bucket::put_object`] 传的对象都有；上传时没带的（比如不经 hygiea 传的）
    /// 报 [`S3Err::MissingSha256`](super::S3Err::MissingSha256)
    async fn get_with_sha256(&self, key: &str) -> Result<Option<(ObjectStream, String)>> {
        let Some((object, sha256_base64)) =
            get_object(self.s3_bucket(), self.request_key(key), true).await?
        else {
            return Ok(None);
        };
        let sha256_base64 = sha256_base64
            .ok_or_else(|| missing_sha256(&object.meta.bucket, &object.meta.full_key))?;
        Ok(Some((object, sha256_base64)))
    }

    /// 对象的元数据。不存在（HeadObject 的 NotFound）时返回 `None`：HeadObject 的响应没有 body，对象不存在和 bucket 不存在分不开。
    /// 不向服务端要内容的 sha256，要的用 [`head_with_sha256`](Self::head_with_sha256)
    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>> {
        let meta = head_object(self.s3_bucket(), self.request_key(key), false).await?;
        Ok(meta.map(|(meta, _)| meta))
    }

    /// 同 [`head`](Self::head)，同时向服务端要内容的 sha256 的 base64，没有的报错，同 [`get_with_sha256`](Self::get_with_sha256)
    async fn head_with_sha256(&self, key: &str) -> Result<Option<(ObjectMeta, String)>> {
        let Some((meta, sha256_base64)) =
            head_object(self.s3_bucket(), self.request_key(key), true).await?
        else {
            return Ok(None);
        };
        let sha256_base64 =
            sha256_base64.ok_or_else(|| missing_sha256(&meta.bucket, &meta.full_key))?;
        Ok(Some((meta, sha256_base64)))
    }

    /// 删除对象。对象不存在也算成功（S3 的 DeleteObject 本来就这样）
    async fn delete(&self, key: &str) -> Result<()> {
        let full_key = self.request_key(key);
        let bucket = self.s3_bucket();
        bucket
            .client
            .delete_object()
            .bucket(&bucket.bucket)
            .key(&full_key)
            .send()
            .await
            .map_err(|e| sdk_error("delete_object", &bucket.bucket, &full_key, e))?;
        Ok(())
    }

    /// GET 的预签名 URL。`expires_in` 是有效期，不传（`None`）用 bucket 配的最长有效期
    /// （[`S3BucketConfig::presign_max_expires_in`](super::S3BucketConfig::presign_max_expires_in)），
    /// 传了的要在 1 秒到它之间，不然报 [`S3Err::InvalidPresignExpiry`](super::S3Err::InvalidPresignExpiry)。
    /// 签名在本地算，不访问服务，也不检查对象在不在。
    /// `options` 签进 URL，读取时覆盖对象上存的响应头（见 [`PresignGetOptions`]），不用覆盖传 `PresignGetOptions::default()`。
    /// 按服务地址签名；客户端配了 [`presign_public_origin`](super::S3ClientConfig::presign_public_origin)、
    /// [`presign_path_prefix`](super::S3ClientConfig::presign_path_prefix) 的，签完换掉 URL 的地址、插上路径前缀
    async fn presigned_get_url(
        &self,
        key: &str,
        expires_in: Option<Duration>,
        options: PresignGetOptions,
    ) -> Result<String> {
        let full_key = self.request_key(key);
        let bucket = self.s3_bucket();
        let request = bucket
            .client
            .get_object()
            .bucket(&bucket.bucket)
            .key(&full_key)
            .set_response_content_type(options.response_content_type)
            .set_response_content_disposition(options.response_content_disposition)
            .set_response_cache_control(options.response_cache_control)
            .presigned(presigning_config(bucket, &full_key, expires_in)?)
            .await
            .map_err(|e| sdk_error("presign_get_object", &bucket.bucket, &full_key, e))?;
        bucket
            .client
            .finish_presigned_url(request.uri().to_string())
    }
}

impl S3Bucket {
    /// 一次 PutObject：把 `body` 写到 `key`，Content-Type 是给的；`sha256_base64`（内容的 sha256 的 base64，
    /// 见 [`PutDigest::sha256_base64`]）、`size` 作为 `x-amz-checksum-sha256`、`Content-Length` 带上，服务端收到的内容和它们对不上就拒绝写入
    /// （报 [`S3Err::ServiceError`](super::S3Err::ServiceError)）。不拼 key、不识别类型、不读内容：
    /// 用在这些都已经知道的地方，比如把 [`put`](S3BucketHandle::put) 传过的对象补传到另一个 S3
    /// （key、sha256、大小、Content-Type 都记在调用方的表里）。
    ///
    /// `body` 可以是内存里的内容（`Bytes`、`Vec<u8>` 等），也可以是别处读出来的流（比如另一个 S3 的
    /// [`get`](S3BucketHandle::get) 的 [`ObjectStream::body`]），边读边传，不进内存。
    /// 流只能读一遍，上传中途失败 SDK 不重试，直接报错；内存里的内容 SDK 会重试。
    ///
    /// 只在 [`S3Bucket`] 上，[`S3BucketWithPrefix`] 没有：现成的 key 是从表里、别的句柄拿来的，
    /// 在带前缀的句柄上很容易把前缀再加一遍；`S3Bucket` 上 key 原样发出，传的就是完整 key
    pub async fn put_object(
        &self,
        key: &str,
        body: impl Into<ByteStream>,
        sha256_base64: &str,
        size: i64,
        content_type: &str,
    ) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(body.into())
            .checksum_sha256(sha256_base64)
            // 流不一定知道自己的长度，都显式带上
            .content_length(size)
            .content_type(content_type)
            .send()
            .await
            .map_err(|e| sdk_error("put_object", &self.bucket, key, e))?;
        Ok(())
    }
}

/// GetObject：对象和响应里内容的 sha256 的 base64（`checksum_mode` 为 true 时向服务端要）。对象不存在（NoSuchKey）返回 `None`
async fn get_object(
    bucket: &S3Bucket,
    full_key: String,
    checksum_mode: bool,
) -> Result<Option<(ObjectStream, Option<String>)>> {
    let output = match bucket
        .client
        .get_object()
        .bucket(&bucket.bucket)
        .key(&full_key)
        .set_checksum_mode(checksum_mode.then_some(ChecksumMode::Enabled))
        .send()
        .await
    {
        Ok(output) => output,
        Err(e) if e.as_service_error().is_some_and(|e| e.is_no_such_key()) => return Ok(None),
        Err(e) => return Err(sdk_error("get_object", &bucket.bucket, &full_key, e)),
    };
    let object = ObjectStream {
        meta: ObjectMeta {
            size: output.content_length,
            content_type: output.content_type,
            etag: output.e_tag,
            bucket: bucket.bucket.clone(),
            full_key,
        },
        body: output.body,
    };
    Ok(Some((object, output.checksum_sha256)))
}

/// HeadObject：元数据和响应里内容的 sha256 的 base64（`checksum_mode` 为 true 时向服务端要）。对象不存在（NotFound）返回 `None`
async fn head_object(
    bucket: &S3Bucket,
    full_key: String,
    checksum_mode: bool,
) -> Result<Option<(ObjectMeta, Option<String>)>> {
    let output = match bucket
        .client
        .head_object()
        .bucket(&bucket.bucket)
        .key(&full_key)
        .set_checksum_mode(checksum_mode.then_some(ChecksumMode::Enabled))
        .send()
        .await
    {
        Ok(output) => output,
        Err(e) if e.as_service_error().is_some_and(|e| e.is_not_found()) => return Ok(None),
        Err(e) => return Err(sdk_error("head_object", &bucket.bucket, &full_key, e)),
    };
    let meta = ObjectMeta {
        size: output.content_length,
        content_type: output.content_type,
        etag: output.e_tag,
        bucket: bucket.bucket.clone(),
        full_key,
    };
    Ok(Some((meta, output.checksum_sha256)))
}

/// 有效期：不传用 bucket 配的最长有效期；传了的要在 1 秒到它之间，不然报 [`S3Err::InvalidPresignExpiry`](super::S3Err::InvalidPresignExpiry)
fn presigning_config(
    bucket: &S3Bucket,
    full_key: &str,
    expires_in: Option<Duration>,
) -> Result<PresigningConfig> {
    let max_expires_in = bucket.presign_max_expires_in;
    let expires_in = expires_in.unwrap_or(max_expires_in);
    ensure!(
        (PRESIGN_MIN_EXPIRES_IN..=max_expires_in).contains(&expires_in),
        from: invalid_presign_expiry(expires_in, max_expires_in, &bucket.bucket, full_key)
    );
    PresigningConfig::expires_in(expires_in).map_err(|e| {
        impossible!("预签名有效期检查过在 1 秒到 {max_expires_in:?} 之间：{expires_in:?}")
            .with_source(e)
    })
}

// ---------------- 上传 ----------------

/// 识别类型时最多看开头多少字节，`infer` 认的格式都在这个范围里
const CONTENT_TYPE_HEAD_LEN: usize = 8 * 1024;

/// 读本地文件时每次读多少字节
const FILE_READ_CHUNK_LEN: usize = 64 * 1024;

/// [`S3BucketHandle::put`] 的 key 里，前缀后面、扩展名前面那一段（主文件名）怎么定
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PutKey {
    /// 内容的 sha256（小写十六进制）：内容一样 key 就一样，调用方可以按 sha256 去重
    Content,
    /// 调用方给的名字，如 `2026-11-02/_SUCCESS`：可以有多层（中间的 `/`），可以有 `.`；
    /// 不能是空的、不能以 `/` 开头、不能有 `//`，不对的在读内容之前就报
    /// [`S3Err::InvalidKey`](super::S3Err::InvalidKey)。和 `Content` 的对象最好放在不同的前缀下
    Named(String),
}

impl PutKey {
    /// [`PutKey::Named`] 的名字按 [`check_key_part`] 的规则检查
    fn check(&self) -> Result<()> {
        match self {
            Self::Content => Ok(()),
            Self::Named(name) => check_key_part("name", name),
        }
    }

    /// 要传的 key（相对句柄）：`key_prefix`（以 `/` 结尾）+ 主文件名 + `.` + 扩展名，没有扩展名的不带 `.`
    fn into_key(self, key_prefix: &str, digest: &PutDigest) -> String {
        let stem = match self {
            Self::Content => digest.sha256_hex(),
            Self::Named(name) => name,
        };
        match &digest.file_ext {
            Some(file_ext) => format!("{key_prefix}{stem}.{file_ext}"),
            None => format!("{key_prefix}{stem}"),
        }
    }
}

/// [`S3BucketHandle::put`] 上传的内容和它的扩展名（可以没有，怎么用见 `put`）。
/// `Bytes`、`Path` 能重复读：上传前先读一遍定 [`PutDigest`]，上传时再读一遍；`Stream` 只能读一遍，摘要由调用方给
#[derive(Debug)]
pub enum PutFile {
    /// 内存里的内容（clone 很便宜，`Bytes` 是引用计数）和扩展名（不带 `.`，如 `mp3`）
    Bytes {
        bytes: Bytes,
        file_ext: Option<String>,
    },
    /// 本地文件的路径，扩展名是路径的扩展名（[`Path::extension`](std::path::Path::extension)，没有的是没有）。
    /// 边读边算、边读边传，不整个读进内存，读不了报 [`S3Err::ReadFileFailed`](super::S3Err::ReadFileFailed)
    Path(PathBuf),
    /// 只能读一遍的流（比如另一个 S3 的 [`get`](S3BucketHandle::get) 读出来的），边读边传，不进内存、不落盘。
    /// 不读内容，摘要由调用方给：`sha256`（32 字节）、`size`（字节数）上传时交给服务端校验，`content_type`、`file_ext`
    /// （不带 `.`，可以没有）照用
    Stream {
        body: ByteStream,
        sha256: [u8; 32],
        size: i64,
        content_type: String,
        file_ext: Option<String>,
    },
}

/// 上传前按内容定下来的：sha256、字节数、Content-Type、扩展名，只能由 [`S3BucketHandle::put`] 算出来
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutDigest {
    sha256: [u8; 32],
    size: i64,
    content_type: String,
    file_ext: Option<String>,
}

impl PutDigest {
    /// 按内容开头最多 [`CONTENT_TYPE_HEAD_LEN`] 字节（`head`）和带的扩展名（检查、转过小写的）定 Content-Type 和扩展名
    /// （见 [`content_type_and_ext`]），和算好的 sha256、字节数一起
    fn new(sha256: [u8; 32], size: i64, head: &[u8], file_ext: Option<&str>) -> Result<Self> {
        let (content_type, file_ext) = content_type_and_ext(head, file_ext)?;
        Ok(Self {
            sha256,
            size,
            content_type,
            file_ext,
        })
    }

    /// 内容的 sha256，小写十六进制，64 个字符
    pub fn sha256_hex(&self) -> String {
        self.sha256
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// 字节数。和 [`ObjectMeta::size`]、AWS SDK 一样用 `i64`
    pub fn size(&self) -> i64 {
        self.size
    }

    /// 上传时设在对象上的 Content-Type，定法见 [`S3BucketHandle::put`]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// 按内容定的小写扩展名（[`S3BucketHandle::put`] 拼在 key 的最后）：文件头认得出来的是认出的，认不出来的是带的，
    /// 都没有的是 `None`。定法见 [`S3BucketHandle::put`]
    pub fn file_ext(&self) -> Option<&str> {
        self.file_ext.as_deref()
    }

    /// 内容的 sha256 的 base64，44 个字符：上传时作为 `x-amz-checksum-sha256` 带上的就是它，
    /// 存下来以后 [`S3Bucket::put_object`] 时原样带上；[`get_with_sha256`](S3BucketHandle::get_with_sha256)、
    /// [`head_with_sha256`](S3BucketHandle::head_with_sha256) 读回来的也是它
    pub fn sha256_base64(&self) -> String {
        BASE64.encode(self.sha256)
    }
}

/// 调用方看过 [`PutDigest`] 后的决定
#[derive(Clone, Debug)]
pub enum PutDecision<T> {
    /// 不上传（比如同样内容存过了），`T` 原样交回给调用方
    Skip(T),
    /// 上传，key 由 [`S3BucketHandle::put`] 按 [`PutKey`] 拼
    Put,
}

/// [`S3BucketHandle::put`] 的结果
#[derive(Clone, Debug)]
pub enum PutOutcome<T> {
    /// 调用方决定不上传，带回 [`PutDecision::Skip`] 里的值
    Skipped(T),
    /// 上传了：key（相对句柄）和上传前定下来的 [`PutDigest`]
    Put { key: String, digest: PutDigest },
}

impl PutFile {
    /// 带的扩展名，转成小写：不能是空的、只能是字母和数字（本地文件的扩展名还要是 UTF-8），
    /// 不对的报 [`S3Err::InvalidFileExt`](super::S3Err::InvalidFileExt)
    fn file_ext(&self) -> Result<Option<String>> {
        let file_ext = match self {
            Self::Bytes { file_ext, .. } | Self::Stream { file_ext, .. } => file_ext.as_deref(),
            Self::Path(path) => match path.extension() {
                Some(file_ext) => Some(
                    file_ext
                        .to_str()
                        .ok_or_else(|| invalid_file_ext(&file_ext.to_string_lossy()))?,
                ),
                None => None,
            },
        };
        let Some(file_ext) = file_ext else {
            return Ok(None);
        };
        ensure!(
            !file_ext.is_empty() && file_ext.chars().all(|c| c.is_ascii_alphanumeric()),
            from: invalid_file_ext(file_ext)
        );
        Ok(Some(file_ext.to_ascii_lowercase()))
    }

    /// 定下 [`PutDigest`]：先检查带的扩展名（见 [`PutFile::file_ext`]），再读内容；内存里的、本地文件读一遍算，
    /// 本地文件按块读，不整个读进内存；流不读，用它带的
    async fn digest(&self) -> Result<PutDigest> {
        let file_ext = self.file_ext()?;
        let file_ext = file_ext.as_deref();
        match self {
            Self::Stream {
                sha256,
                size,
                content_type,
                ..
            } => Ok(PutDigest {
                sha256: *sha256,
                size: *size,
                content_type: content_type.clone(),
                file_ext: file_ext.map(str::to_owned),
            }),
            Self::Bytes { bytes, .. } => {
                let head = &bytes[..bytes.len().min(CONTENT_TYPE_HEAD_LEN)];
                // 内存里的长度不超过 isize::MAX，转 i64 不会丢
                PutDigest::new(
                    Sha256::digest(bytes).into(),
                    bytes.len() as i64,
                    head,
                    file_ext,
                )
            }
            Self::Path(path) => {
                let read_failed = |e| read_file_failed(&path.display().to_string(), e);
                let mut file = tokio::fs::File::open(path).await.map_err(read_failed)?;
                let mut hasher = Sha256::new();
                let mut buffer = vec![0; FILE_READ_CHUNK_LEN];
                let mut head = Vec::with_capacity(CONTENT_TYPE_HEAD_LEN);
                let mut size: i64 = 0;
                loop {
                    let read = file.read(&mut buffer).await.map_err(read_failed)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                    let head_missing = CONTENT_TYPE_HEAD_LEN - head.len();
                    head.extend_from_slice(&buffer[..read.min(head_missing)]);
                    // 一次最多读满 buffer，转 i64 不会丢
                    size += read as i64;
                }
                PutDigest::new(hasher.finalize().into(), size, &head, file_ext)
            }
        }
    }

    /// 上传用的 body：内存里的、本地文件重新建一个（SDK 重试时能重新读），流交出去
    async fn into_byte_stream(self) -> Result<ByteStream> {
        match self {
            Self::Bytes { bytes, .. } => Ok(ByteStream::from(bytes)),
            Self::Path(path) => ByteStream::from_path(&path)
                .await
                .map_err(|e| read_file_failed(&path.display().to_string(), e)),
            Self::Stream { body, .. } => Ok(body),
        }
    }
}

/// 按内容开头的字节（`head`）和带的扩展名（检查、转过小写的）定 Content-Type 和扩展名：
///
/// - 按文件头（magic number）认得出来的：给了扩展名的要和它对得上（扩展名一样，或者扩展名对应的类型里有它），
///   对不上报 [`S3Err::ContentTypeMismatch`](super::S3Err::ContentTypeMismatch)；结果是认出的类型和它的扩展名
/// - 认不出来的（纯文本这类没有固定文件头的）：扩展名用给的，类型按它推断；推断不出、没给的是 `application/octet-stream`
fn content_type_and_ext(head: &[u8], file_ext: Option<&str>) -> Result<(String, Option<String>)> {
    let Some(detected) = infer::get(head) else {
        let guessed = file_ext.and_then(|file_ext| mime_guess::from_ext(file_ext).first());
        let content_type = match guessed {
            Some(mime) => mime.to_string(),
            None => mime_guess::mime::APPLICATION_OCTET_STREAM.to_string(),
        };
        return Ok((content_type, file_ext.map(str::to_owned)));
    };
    if let Some(file_ext) = file_ext {
        let matches_ext = detected.extension().eq_ignore_ascii_case(file_ext)
            || mime_guess::from_ext(file_ext)
                .iter()
                .any(|mime| mime.essence_str() == detected.mime_type());
        ensure!(
            matches_ext,
            from: content_type_mismatch(file_ext, detected.mime_type())
        );
    }
    Ok((
        detected.mime_type().to_owned(),
        Some(detected.extension().to_ascii_lowercase()),
    ))
}

// ---------------- key 前缀 ----------------

/// S3 key 的前缀，可以有多层，如 `myapp/audio`。用在上传（[`S3BucketHandle::put`] 的 `key_prefix`）
/// 和带前缀的句柄（[`S3Bucket::with_prefix`]）上。
///
/// hygiea 给 `str`、`String` 和它们的引用实现了，可以直接传字符串；使用方也可以给自己的类型实现，比如列出全部文件类型的枚举，
/// 调用的地方就只能传定义好的类型：
///
/// ```ignore
/// enum FileKind { Audio, Image }
///
/// impl S3KeyPrefix for FileKind {
///     fn key_prefix(&self) -> &str {
///         match self {
///             Self::Audio => "myapp/audio",
///             Self::Image => "myapp/image",
///         }
///     }
/// }
///
/// bucket.put(FileKind::Audio, PutKey::Content, file, decide).await?;   // 实现了它的类型，引用也能传（&FileKind::Audio）
/// ```
///
/// 用的时候检查、规整，不符合规则的报 [`S3Err::InvalidKey`](super::S3Err::InvalidKey)：
///
/// - 不能是空的：没有前缀就不用它
/// - 不能以 `/` 开头、中间不能有 `//`：控制台会多出名字为空的文件夹、URL 里出现双斜杠，是写错了
/// - 结尾没有 `/` 的补上，否则前缀和 key 连成一个名字（`app` + `a.mp3` = `appa.mp3`），比如 `myapp` → `myapp/`
pub trait S3KeyPrefix {
    /// 前缀，结尾的 `/` 可写可不写
    fn key_prefix(&self) -> &str;
}

impl S3KeyPrefix for str {
    fn key_prefix(&self) -> &str {
        self
    }
}

impl S3KeyPrefix for String {
    fn key_prefix(&self) -> &str {
        self
    }
}

/// 引用也算：`&str`、`&String`、使用方类型的引用都能直接传
impl<T: S3KeyPrefix + ?Sized> S3KeyPrefix for &T {
    fn key_prefix(&self) -> &str {
        (**self).key_prefix()
    }
}

/// key 的一段（[`S3KeyPrefix`] 的前缀、[`PutKey::Named`] 的名字）的规则：不能是空的、不能以 `/` 开头、不能有 `//`。
/// 不符合的报 [`S3Err::InvalidKey`](super::S3Err::InvalidKey)，`part` 是哪一段（`prefix` / `name`）
fn check_key_part(part: &str, value: &str) -> Result<()> {
    ensure!(!value.is_empty(), from: invalid_key(part, value, "empty"));
    ensure!(
        !value.starts_with('/'),
        from: invalid_key(part, value, "starts with '/'")
    );
    ensure!(
        !value.contains("//"),
        from: invalid_key(part, value, "contains '//'")
    );
    Ok(())
}

/// 按 [`S3KeyPrefix`] 的规则检查（见 [`check_key_part`]），返回以 `/` 结尾的前缀
fn normalized_key_prefix(prefix: impl S3KeyPrefix) -> Result<String> {
    let prefix = prefix.key_prefix();
    check_key_part("prefix", prefix)?;
    if prefix.ends_with('/') {
        Ok(prefix.to_owned())
    } else {
        Ok(format!("{prefix}/"))
    }
}

// ---------------- 读、预签名用的类型 ----------------

/// 预签名 GET 的选项：签进 URL（`response-*` 参数），读取时服务端用它们覆盖对象上存的响应头，没设的不带、用对象上存的。
/// 签在 URL 里，拿到 URL 的人改不了。要指定文件名的用 [`attachment`](Self::attachment)、[`inline`](Self::inline) 建，
/// 再设别的项用结构体更新语法：`PresignGetOptions { response_cache_control: Some(..), ..PresignGetOptions::attachment(name) }`
#[derive(Clone, Debug, Default)]
pub struct PresignGetOptions {
    pub response_content_type: Option<String>,
    /// 浏览器直接打开还是下载、下载时的文件名，如 `attachment; filename="a.mp3"`。原样签进 URL，
    /// 文件名有非 ASCII、引号等字符的要自己按 RFC 6266 编码，用 [`attachment`](Self::attachment)、[`inline`](Self::inline) 就不用管
    pub response_content_disposition: Option<String>,
    pub response_cache_control: Option<String>,
}

impl PresignGetOptions {
    /// 浏览器下载，另存的文件名是 `file_name`（原始的名字，不用编码）。Content-Disposition 写成
    /// `attachment; filename="<后备名>"; filename*=UTF-8''<编码后的名字>`：现在的浏览器认 `filename*`，用完整的名字
    /// （按 UTF-8 字节写成 `%XX`，RFC 5987）；不认的老客户端用后备名（`"`、`\`、控制字符、非 ASCII 字符换成 `_`）。
    /// 文件名是空的只写 `attachment`
    pub fn attachment(file_name: &str) -> Self {
        Self {
            response_content_disposition: Some(content_disposition("attachment", file_name)),
            ..Self::default()
        }
    }

    /// 浏览器直接打开，另存时的文件名是 `file_name`，同 [`attachment`](Self::attachment)
    pub fn inline(file_name: &str) -> Self {
        Self {
            response_content_disposition: Some(content_disposition("inline", file_name)),
            ..Self::default()
        }
    }
}

/// Content-Disposition（RFC 6266）：`<kind>; filename="<后备名>"; filename*=UTF-8''<编码后的名字>`。
/// 认 `filename*` 的客户端（现在的浏览器都认）用完整的名字：字母、数字和 ``!#$&+-.^_`|~`` 照留，其他按 UTF-8 字节写成 `%XX`（RFC 5987）；
/// 不认的老客户端用后备名：可打印的 ASCII 和空格照留，`"`、`\`、控制字符、非 ASCII 字符换成 `_`。
/// 文件名是空的只写 `kind`
fn content_disposition(kind: &str, file_name: &str) -> String {
    if file_name.is_empty() {
        return kind.to_owned();
    }
    let fallback: String = file_name
        .chars()
        .map(|c| {
            let printable = c == ' ' || (c.is_ascii_graphic() && c != '"' && c != '\\');
            if printable { c } else { '_' }
        })
        .collect();
    let encoded = utf8_percent_encode(file_name, FILENAME_EXT_ENCODE);
    format!("{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// `filename*` 里要编码的字符：RFC 5987 的 attr-char（字母、数字和 ``!#$&+-.^_`|~``）之外的
const FILENAME_EXT_ENCODE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'#')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b'-')
    .remove(b'.')
    .remove(b'^')
    .remove(b'_')
    .remove(b'`')
    .remove(b'|')
    .remove(b'~');

/// 对象的元数据（HeadObject、GetObject 响应里的），服务端没返回的项是 `None`
#[derive(Clone, Debug)]
pub struct ObjectMeta {
    /// 字节数（响应的 Content-Length）
    pub size: Option<i64>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    /// 实际的 bucket 名、完整 key：读内容出错时带上，`Debug` 里也有。不公开，免得完整 key 被传回带前缀的句柄
    bucket: String,
    full_key: String,
}

/// 对象：元数据和内容的字节流（[`S3BucketHandle::get`] 的结果）
#[derive(Debug)]
pub struct ObjectStream {
    pub meta: ObjectMeta,
    /// 内容，边读边处理，读多少由调用方控制：`into_async_read()` 当 `AsyncBufRead`（比如 `tokio::io::copy` 写进文件），
    /// `into_inner()` 当 http-body 1.x 的 Body（比如直接写进 HTTP 响应）；要整段内容的用 [`bytes`](Self::bytes)
    pub body: ByteStream,
}

impl ObjectStream {
    /// 把内容整个读进内存，适合小对象；大对象直接用 [`body`](Self::body) 边读边处理。
    /// 读的过程中出错（连接断开等）报 [`S3Err::RequestFailed`](super::S3Err::RequestFailed)，`op` 是 `get_object`
    pub async fn bytes(self) -> Result<Bytes> {
        let body =
            self.body.collect().await.map_err(|e| {
                request_failed("get_object", &self.meta.bucket, &self.meta.full_key, e)
            })?;
        Ok(body.into_bytes())
    }
}

// ---------------- 管理 bucket ----------------

/// 管理 bucket（要开 `s3-bucket-admin` feature，客户端要开 `manage_buckets`）
#[cfg(feature = "s3-bucket-admin")]
impl S3Bucket {
    /// 删掉这个 bucket。bucket 要是空的（S3 本身的要求），不空时服务端报错（`ServiceError`，S3 错误码 `BucketNotEmpty`）。
    /// 客户端没开 `manage_buckets` 报 [`S3Err::BucketAdminDisabled`](super::S3Err::BucketAdminDisabled)
    pub async fn delete_bucket(&self) -> Result<()> {
        self.ensure_manage_buckets()?;
        self.delete_empty_bucket().await
    }

    /// 先删光 bucket 里的对象，再删 bucket。按页列对象（一页最多 1000 个），
    /// 每页用一次 DeleteObjects 删。开了版本控制的 bucket 只删得掉当前版本，旧版本还在，最后删 bucket 时服务端报错。
    /// 有对象没删掉报 [`S3Err::DeleteObjectsFailed`](super::S3Err::DeleteObjectsFailed)；
    /// 客户端没开 `manage_buckets` 报 [`S3Err::BucketAdminDisabled`](super::S3Err::BucketAdminDisabled)
    pub async fn empty_and_delete_bucket(&self) -> Result<()> {
        use aws_sdk_s3::types::{Delete, ObjectIdentifier};

        self.ensure_manage_buckets()?;
        let mut continuation_token = None;
        loop {
            let page = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .set_continuation_token(continuation_token.take())
                .send()
                .await
                .map_err(|e| sdk_error("list_objects_v2", &self.bucket, "", e))?;
            let mut identifiers = Vec::with_capacity(page.contents().len());
            for object in page.contents() {
                let key = object.key().ok_or_else(|| {
                    invalid_response("list_objects_v2", &self.bucket, "object key")
                })?;
                identifiers.push(
                    ObjectIdentifier::builder()
                        .key(key)
                        .build()
                        .map_err(|e| impossible!("ObjectIdentifier 只要求 key，已经设了：{e}"))?,
                );
            }
            if !identifiers.is_empty() {
                let delete = Delete::builder()
                    .set_objects(Some(identifiers))
                    .quiet(true)
                    .build()
                    .map_err(|e| impossible!("Delete 只要求 objects，已经设了：{e}"))?;
                let output = self
                    .client
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(delete)
                    .send()
                    .await
                    .map_err(|e| sdk_error("delete_objects", &self.bucket, "", e))?;
                if let Some(first) = output.errors().first() {
                    bail!(from: delete_objects_failed(
                        &self.bucket,
                        output.errors().len(),
                        first.key(),
                        first.code(),
                    ));
                }
            }
            if page.is_truncated() != Some(true) {
                break;
            }
            let Some(token) = page.next_continuation_token() else {
                bail!(from: invalid_response("list_objects_v2", &self.bucket, "next continuation token"));
            };
            continuation_token = Some(token.to_owned());
        }
        self.delete_empty_bucket().await
    }

    fn ensure_manage_buckets(&self) -> Result<()> {
        if self.client.manage_buckets() {
            Ok(())
        } else {
            Err(bucket_admin_disabled(self.client.name(), &self.bucket))
        }
    }

    async fn delete_empty_bucket(&self) -> Result<()> {
        self.client
            .delete_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map_err(|e| sdk_error("delete_bucket", &self.bucket, "", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::S3Err;

    /// PNG 文件头、mp3 的帧头（不带 ID3，开头 0xFF 0xF3）
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const MP3: &[u8] = b"\xff\xf3\x44\xc4\0\0\0\0";

    /// 认得出来、和给的扩展名对得上（或者没给）：用认出的类型和它的扩展名
    #[test]
    fn detected_matching_ext() {
        let png = ("image/png".to_owned(), Some("png".to_owned()));
        assert_eq!(content_type_and_ext(PNG, Some("png")).unwrap(), png);
        assert_eq!(content_type_and_ext(PNG, None).unwrap(), png);
        assert_eq!(
            content_type_and_ext(MP3, Some("mp3")).unwrap(),
            ("audio/mpeg".to_owned(), Some("mp3".to_owned()))
        );
    }

    /// 认得出来、和给的扩展名对不上：报 ContentTypeMismatch
    #[test]
    fn detected_mismatching_ext() {
        let err = content_type_and_ext(PNG, Some("mp3")).unwrap_err();
        assert!(err.is(S3Err::ContentTypeMismatch), "{err:#}");
    }

    /// 认不出来的用给的扩展名，类型按它推断；推断不出、没给的是 octet-stream
    #[test]
    fn falls_back_to_ext() {
        assert_eq!(
            content_type_and_ext(b"hello", Some("txt")).unwrap(),
            ("text/plain".to_owned(), Some("txt".to_owned()))
        );
        assert_eq!(
            content_type_and_ext(b"hello", Some("unknownext")).unwrap(),
            (
                "application/octet-stream".to_owned(),
                Some("unknownext".to_owned())
            )
        );
        assert_eq!(
            content_type_and_ext(b"hello", None).unwrap(),
            ("application/octet-stream".to_owned(), None)
        );
    }

    /// 带的扩展名：内存里的用给的，本地文件用路径的扩展名（没有的是 None）；转成小写，不合规则的报 InvalidFileExt
    #[test]
    fn file_ext_of_file() {
        let bytes = |file_ext: Option<&str>| PutFile::Bytes {
            bytes: Bytes::new(),
            file_ext: file_ext.map(str::to_owned),
        };
        assert_eq!(
            bytes(Some("MP3")).file_ext().unwrap().as_deref(),
            Some("mp3")
        );
        assert_eq!(bytes(None).file_ext().unwrap(), None);
        let path = |path: &str| PutFile::Path(path.into());
        assert_eq!(
            path("/tmp/さくら.MP3").file_ext().unwrap().as_deref(),
            Some("mp3")
        );
        assert_eq!(
            path("/tmp/a.tar.gz").file_ext().unwrap().as_deref(),
            Some("gz")
        );
        for no_ext in ["/tmp/noext", "/tmp/.bashrc", "/"] {
            assert_eq!(path(no_ext).file_ext().unwrap(), None, "{no_ext}");
        }
        for invalid in ["", ".mp3", "m-p3", "mp 3", "mp\n3"] {
            let err = bytes(Some(invalid)).file_ext().unwrap_err();
            assert!(err.is(S3Err::InvalidFileExt), "{invalid:?}: {err:#}");
        }
        let err = path("/tmp/a.m-p3").file_ext().unwrap_err();
        assert!(err.is(S3Err::InvalidFileExt), "{err:#}");
    }

    /// 流不读内容，摘要用它带的；扩展名照样转小写
    #[tokio::test]
    async fn digest_of_stream() {
        let file = PutFile::Stream {
            body: ByteStream::from_static(b"not read"),
            sha256: [7; 32],
            size: 5,
            content_type: "audio/mpeg".into(),
            file_ext: Some("MP3".into()),
        };
        let digest = file.digest().await.unwrap();
        assert_eq!(digest.sha256_hex(), "07".repeat(32));
        assert_eq!(digest.size(), 5);
        assert_eq!(digest.content_type(), "audio/mpeg");
        assert_eq!(digest.file_ext(), Some("mp3"));
    }

    /// 内存里的和本地文件算出的摘要一样（`echo -n hello | shasum -a 256`）；类型按开头的字节认，
    /// 认得出来的格式在开头 CONTENT_TYPE_HEAD_LEN 字节以内，后面多长都一样；文件读不了报 ReadFileFailed
    #[tokio::test]
    async fn digest_of_bytes_and_file() {
        const HELLO_SHA256: &str =
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let digest = PutFile::Bytes {
            bytes: Bytes::from_static(b"hello"),
            file_ext: Some("txt".into()),
        }
        .digest()
        .await
        .unwrap();
        assert_eq!(digest.sha256_hex(), HELLO_SHA256);
        assert_eq!(digest.size(), 5);
        assert_eq!(digest.content_type(), "text/plain");
        assert_eq!(digest.file_ext(), Some("txt"));
        assert_eq!(
            digest.sha256_base64(),
            "LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ="
        );

        let mut file = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
        file.write_all(b"hello").unwrap();
        let file_digest = PutFile::Path(file.path().to_owned())
            .digest()
            .await
            .unwrap();
        assert_eq!(file_digest, digest);

        let mut big = PNG.to_vec();
        big.resize(CONTENT_TYPE_HEAD_LEN * 3, 7);
        let big_digest = PutFile::Bytes {
            bytes: Bytes::from(big.clone()),
            file_ext: None,
        }
        .digest()
        .await
        .unwrap();
        assert_eq!(big_digest.content_type(), "image/png");
        let mut big_file = tempfile::NamedTempFile::new().unwrap();
        big_file.write_all(&big).unwrap();
        let big_file_digest = PutFile::Path(big_file.path().to_owned())
            .digest()
            .await
            .unwrap();
        assert_eq!(big_file_digest, big_digest);

        let err = PutFile::Path("/no/such/file".into())
            .digest()
            .await
            .unwrap_err();
        assert!(err.is(S3Err::ReadFileFailed), "{err:#}");
    }

    /// ASCII 的名字后备名和 filename* 都是原样；非 ASCII、引号、反斜杠、控制字符在后备名里换成 _，
    /// filename* 里按 UTF-8 字节写成 %XX；空的只有类型
    #[test]
    fn content_disposition_encodes_file_name() {
        assert_eq!(
            content_disposition("attachment", "a.mp3"),
            r#"attachment; filename="a.mp3"; filename*=UTF-8''a.mp3"#
        );
        assert_eq!(
            content_disposition("inline", "さくら 1.mp3"),
            r#"inline; filename="___ 1.mp3"; filename*=UTF-8''%E3%81%95%E3%81%8F%E3%82%89%201.mp3"#
        );
        assert_eq!(
            content_disposition("attachment", "a\"b\\c\nd.txt"),
            r#"attachment; filename="a_b_c_d.txt"; filename*=UTF-8''a%22b%5Cc%0Ad.txt"#
        );
        assert_eq!(content_disposition("attachment", ""), "attachment");
    }

    /// key 的一段：可以有多层、可以有 `.`；空的、以 `/` 开头、有 `//` 的报 InvalidKey
    #[test]
    fn checks_key_part() {
        for valid in ["a", "a/b", "a.b", "2026-11-02/_SUCCESS", "a/"] {
            check_key_part("name", valid).unwrap();
        }
        for invalid in ["", "/a", "a//b"] {
            let err = check_key_part("name", invalid).unwrap_err();
            assert!(err.is(S3Err::InvalidKey), "{invalid:?}: {err:#}");
        }
    }

    /// 结尾没有 `/` 的补上
    #[test]
    fn normalizes_trailing_slash() {
        for (given, expected) in [
            ("app", "app/"),
            ("app/", "app/"),
            ("app/audio", "app/audio/"),
        ] {
            assert_eq!(normalized_key_prefix(given).unwrap(), expected, "{given:?}");
        }
    }

    /// 空的、以 `/` 开头的、中间有 `//` 的报错
    #[test]
    fn rejects_invalid() {
        for given in ["", "/", "/app/", "//app", "app//audio", "app//"] {
            let err = normalized_key_prefix(given).unwrap_err();
            assert!(err.is(S3Err::InvalidKey), "{given:?}: {err:#}");
        }
    }

    /// `String`、使用方自己的类型和它们的引用，按给的前缀检查
    #[test]
    fn other_implementations() {
        enum FileKind {
            Audio,
            Broken,
        }

        impl S3KeyPrefix for FileKind {
            fn key_prefix(&self) -> &str {
                match self {
                    Self::Audio => "app/audio",
                    Self::Broken => "/app",
                }
            }
        }

        let app = "app".to_owned();
        assert_eq!(normalized_key_prefix(&app).unwrap(), "app/");
        assert_eq!(normalized_key_prefix(app).unwrap(), "app/");
        assert_eq!(
            normalized_key_prefix(FileKind::Audio).unwrap(),
            "app/audio/"
        );
        assert_eq!(
            normalized_key_prefix(&FileKind::Audio).unwrap(),
            "app/audio/"
        );
        let err = normalized_key_prefix(FileKind::Broken).unwrap_err();
        assert!(err.is(S3Err::InvalidKey), "{err:#}");
    }
}
