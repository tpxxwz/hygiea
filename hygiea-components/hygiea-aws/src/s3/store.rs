//! [`AwsS3Store`]：按配置建好的全部客户端和 bucket，组件 [`AwsS3Component`] 放进 Resources 的资源；
//! 按段名取 bucket 时的段名实现 [`S3BucketName`]。

use std::collections::BTreeMap;

use aws_sdk_s3::types::CreateBucketConfiguration;
use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, RegistryConfig, async_trait,
};
use hygiea_core::{Result, bail, ensure};
use indexmap::{IndexMap, IndexSet};

use super::client::AwsS3Client;
use super::config::{
    DEFAULT_BUCKET_SECTION, DEFAULT_CLIENT_SECTION, PRESIGN_MAX_EXPIRES_IN, PRESIGN_MIN_EXPIRES_IN,
};
use super::error::{
    bucket_create_failed, bucket_not_configured, bucket_not_found, client_not_configured, sdk_error,
};
use super::{S3Bucket, S3BucketConfig, S3ClientConfig, S3Config};

/// 配置里的全部客户端和 bucket，启动时按 [`S3Config`] 建好。客户端和客户端下的 bucket 两层都是「一个默认的 + 按名字的其他」，
/// 取 bucket 句柄的方法按要哪个客户端、哪些 bucket 分，默认的只从默认的方法取，按名字取的结果里不会混进默认的：
///
/// | | 默认 bucket | 按段名的其他 bucket |
/// |---|---|---|
/// | 默认客户端 | [`default_client_default_bucket`](Self::default_client_default_bucket)，大多数应用只用它 | [`default_client_buckets`](Self::default_client_buckets) |
/// | 一个其他客户端 | [`client_default_bucket`](Self::client_default_bucket) | [`client_buckets`](Self::client_buckets) |
/// | 几个其他客户端 | [`clients_default_buckets`](Self::clients_default_buckets) | [`clients_buckets`](Self::clients_buckets) |
///
/// 段名可以是字符串，也可以是使用方实现了 [`S3BucketName`] 的类型。只能用配置里的（bucket 启动时检查过存在）：
///
/// ```ignore
/// let store = resources.require::<AwsS3Store>()?;
/// let files = store.default_client_default_bucket();            // key 原样发出
/// let audio = files.with_prefix("myapp/audio")?;                // key 都加上 myapp/audio/
/// let backup = store.client_default_bucket("r2")?;
/// let targets = store.clients_default_buckets(&IndexSet::from(["r2".to_owned()]))?;   // {r2}
/// ```
///
/// 建的时候（[`ConfigResource::from_config`]）先检查配置，再逐个检查 bucket，任何一项不通过都返回错误、组件启动失败：
///
/// - 其他客户端的名字、其他 bucket 的段名不能是空的，也不能叫 `default`
/// - 实际的 bucket 名（[`S3BucketConfig::name`](super::S3BucketConfig::name)，没写是段名）不能是空的，同一个客户端下不能重复；
///   写了的 `name` 不能和同一个客户端下别的段的段名一样（默认 bucket 的段名是 `default`）；预签名的最长有效期在 1 秒到 7 天之间
/// - 每个 bucket 发一次 HeadBucket：存在就用；不存在时所在的客户端开了 `manage_buckets` 就建（同时启动的另一个实例先建好了也算成功），
///   没开报 [`S3Err::BucketNotFound`](super::S3Err::BucketNotFound)，建失败（比如没有建 bucket 的权限）报
///   [`S3Err::BucketCreateFailed`](super::S3Err::BucketCreateFailed)；HeadBucket 没权限（403）、网络错误等直接报错
#[derive(Clone, Debug)]
pub struct AwsS3Store {
    default: S3ClientBuckets,
    /// 客户端名 → 它下面的 bucket
    extra: BTreeMap<String, S3ClientBuckets>,
}

/// 一个客户端下配置的 bucket：默认的和按段名的其他 bucket
#[derive(Clone, Debug)]
struct S3ClientBuckets {
    /// 客户端名，默认的是 `default`，报错时用
    name: String,
    default: S3Bucket,
    /// 段名 → 句柄（发请求用实际的 bucket 名）
    extra: BTreeMap<String, S3Bucket>,
}

impl AwsS3Store {
    /// 默认客户端（配置里 `[<段名>.default]`）的默认 bucket（`[<段名>.default.buckets.default]`）的句柄。
    /// 两个都必须配，所以一定有。key 原样发出，要限定在一个前缀下用 [`S3Bucket::with_prefix`]
    pub fn default_client_default_bucket(&self) -> S3Bucket {
        self.default.default.clone()
    }

    /// 默认客户端上，`buckets` 里每个其他 bucket（配置里 `[<段名>.default.buckets.extra.<段名>]`）的句柄：
    /// key 是段名，按 `buckets` 的顺序。`buckets` 里有没配的段名（包括写成 `default`）报
    /// [`S3Err::BucketNotConfigured`](super::S3Err::BucketNotConfigured)
    pub fn default_client_buckets<B: S3BucketName>(
        &self,
        buckets: &IndexSet<B>,
    ) -> Result<IndexMap<String, S3Bucket>> {
        self.default.selected_buckets(buckets)
    }

    /// 名为 `client` 的其他客户端（配置里 `[<段名>.extra.<client>]`）的默认 bucket（`.buckets.default`）的句柄。
    /// 没配这个客户端（包括写成 `default`）报 [`S3Err::ClientNotConfigured`](super::S3Err::ClientNotConfigured)
    pub fn client_default_bucket(&self, client: &str) -> Result<S3Bucket> {
        Ok(self.extra_client(client)?.default.clone())
    }

    /// 名为 `client` 的其他客户端上，`buckets` 里每个其他 bucket（配置里 `.buckets.extra.<段名>`）的句柄：
    /// key 是段名，按 `buckets` 的顺序。
    /// 没配这个客户端（包括写成 `default`）报 [`S3Err::ClientNotConfigured`](super::S3Err::ClientNotConfigured)，
    /// `buckets` 里有没配的段名（包括写成 `default`）报 [`S3Err::BucketNotConfigured`](super::S3Err::BucketNotConfigured)
    pub fn client_buckets<B: S3BucketName>(
        &self,
        client: &str,
        buckets: &IndexSet<B>,
    ) -> Result<IndexMap<String, S3Bucket>> {
        self.extra_client(client)?.selected_buckets(buckets)
    }

    /// `extra_clients` 里每个其他客户端的默认 bucket 的句柄：key 是客户端名，按 `extra_clients` 的顺序。
    /// 同一个文件要存进几个 S3 时用，一次检查完用到的客户端，key 可以直接存起来，之后按名字再取
    /// （[`client_default_bucket`](Self::client_default_bucket)）。
    /// `extra_clients` 里有没配的客户端（包括写成 `default`）报 [`S3Err::ClientNotConfigured`](super::S3Err::ClientNotConfigured)
    pub fn clients_default_buckets(
        &self,
        extra_clients: &IndexSet<String>,
    ) -> Result<IndexMap<String, S3Bucket>> {
        extra_clients
            .iter()
            .map(|client| Ok((client.clone(), self.client_default_bucket(client)?)))
            .collect()
    }

    /// `extra_clients` 里每个其他客户端上，各取 [`client_buckets`](Self::client_buckets) 那样的几个 bucket：
    /// 外层 key 是客户端名，按 `extra_clients` 的顺序；内层 key 是段名，按 `buckets` 的顺序。每个客户端下都要配了 `buckets` 里的段名。
    /// 没配的客户端（包括写成 `default`）报 [`S3Err::ClientNotConfigured`](super::S3Err::ClientNotConfigured)，
    /// 有客户端下没配某个段名（包括写成 `default`）报 [`S3Err::BucketNotConfigured`](super::S3Err::BucketNotConfigured)
    pub fn clients_buckets<B: S3BucketName>(
        &self,
        extra_clients: &IndexSet<String>,
        buckets: &IndexSet<B>,
    ) -> Result<IndexMap<String, IndexMap<String, S3Bucket>>> {
        extra_clients
            .iter()
            .map(|client| Ok((client.clone(), self.client_buckets(client, buckets)?)))
            .collect()
    }

    fn extra_client(&self, client: &str) -> Result<&S3ClientBuckets> {
        self.extra
            .get(client)
            .ok_or_else(|| client_not_configured(client))
    }
}

impl S3ClientBuckets {
    /// `buckets` 里每个其他 bucket 的句柄，key 是段名，按 `buckets` 的顺序。有没配的段名（包括写成 `default`）报
    /// [`S3Err::BucketNotConfigured`](super::S3Err::BucketNotConfigured)
    fn selected_buckets<B: S3BucketName>(
        &self,
        buckets: &IndexSet<B>,
    ) -> Result<IndexMap<String, S3Bucket>> {
        let mut handles = IndexMap::with_capacity(buckets.len());
        for bucket in buckets {
            let section = bucket.bucket_name();
            let Some(handle) = self.extra.get(section) else {
                return Err(bucket_not_configured(&self.name, section));
            };
            handles.insert(section.to_owned(), handle.clone());
        }
        Ok(handles)
    }

    /// 建客户端，逐个检查（或建）bucket，建好句柄
    async fn from_config(
        name: &str,
        config: &S3ClientConfig,
        registry_config: &RegistryConfig,
    ) -> Result<Self> {
        let client = AwsS3Client::from_config(name, config, registry_config).await?;
        let bucket = async |section: &str, bucket_config: &S3BucketConfig| -> Result<S3Bucket> {
            let bucket_name = bucket_config.bucket_name(section);
            ensure_bucket(name, &client, bucket_name).await?;
            Ok(S3Bucket::new(
                client.clone(),
                bucket_name.to_owned(),
                bucket_config.presign_max_expires_in,
            ))
        };
        let default = bucket(DEFAULT_BUCKET_SECTION, &config.buckets.default).await?;
        let mut extra = BTreeMap::new();
        for (section, bucket_config) in &config.buckets.extra {
            extra.insert(section.clone(), bucket(section, bucket_config).await?);
        }
        Ok(Self {
            name: name.to_owned(),
            default,
            extra,
        })
    }
}

#[async_trait]
impl ConfigResource for AwsS3Store {
    type Config = S3Config;

    async fn from_config(config: &S3Config, registry_config: &RegistryConfig) -> Result<Self> {
        check_config(config)?;
        let default =
            S3ClientBuckets::from_config(DEFAULT_CLIENT_SECTION, &config.default, registry_config)
                .await?;
        let mut extra = BTreeMap::new();
        for (name, client_config) in &config.extra {
            let client = S3ClientBuckets::from_config(name, client_config, registry_config).await?;
            extra.insert(name.clone(), client);
        }
        Ok(Self { default, extra })
    }
}

/// S3 组件，放进 Resources 的是 [`AwsS3Store`]。用法见 [`ImmediateResourceComponent`]
pub type AwsS3Component = ImmediateResourceComponent<AwsS3Store>;

/// 配置里的哪个其他 bucket：客户端下 `buckets.extra.<段名>` 的段名（服务上实际的 bucket 名是段里的
/// [`name`](super::S3BucketConfig::name)）。按段名取客户端下的其他 bucket 时用
/// （[`AwsS3Store::default_client_buckets`]、[`AwsS3Store::client_buckets`]、[`AwsS3Store::clients_buckets`]）。
///
/// hygiea 给 `str`、`String` 和它们的引用实现了，可以直接传字符串；使用方也可以给自己的类型实现，
/// 比如列出全部 bucket 的枚举，调用的地方就只能传定义好的：
///
/// ```ignore
/// enum AppBucket { Files }
///
/// impl S3BucketName for AppBucket {
///     fn bucket_name(&self) -> &str {
///         match self {
///             Self::Files => "files",
///         }
///     }
/// }
///
/// let buckets = store.client_buckets("r2", &IndexSet::from([AppBucket::Files]))?;
/// ```
pub trait S3BucketName {
    /// 段名
    fn bucket_name(&self) -> &str;
}

impl S3BucketName for str {
    fn bucket_name(&self) -> &str {
        self
    }
}

impl S3BucketName for String {
    fn bucket_name(&self) -> &str {
        self
    }
}

/// 引用也算：`&str`、`&String`、使用方类型的引用都能直接传
impl<T: S3BucketName + ?Sized> S3BucketName for &T {
    fn bucket_name(&self) -> &str {
        (**self).bucket_name()
    }
}

/// 不发请求的检查：其他客户端的名字不是空的、不叫 `default`；同一个客户端下：其他 bucket 的段名不是空的、不叫 `default`；
/// 预签名的最长有效期在 1 秒到 7 天之间；实际的 bucket 名不是空的、不重复；写了的 `name` 不和别的段的段名一样
/// （那个段没写 `name`、实际的 bucket 是另一个也不行，按段名取的和实际叫这个名字的不是同一个 bucket，读配置时容易看错）。
/// 客户端名、同一个客户端下的段名是配置里 map 的 key，配置文件里写重了解析时就报错，不用这里查
fn check_config(config: &S3Config) -> Result<()> {
    ensure!(
        !config.extra.contains_key(""),
        AppErr::InvalidConfig,
        "s3 extra client has empty name"
    );
    ensure!(
        !config.extra.contains_key(DEFAULT_CLIENT_SECTION),
        AppErr::InvalidConfig,
        format!(
            "s3 extra client can't be named {DEFAULT_CLIENT_SECTION:?}, it's the default client's name"
        )
    );
    for (client_name, client_config) in
        default_and_extra(DEFAULT_CLIENT_SECTION, &config.default, &config.extra)
    {
        let buckets = &client_config.buckets;
        ensure!(
            !buckets.extra.contains_key(""),
            AppErr::InvalidConfig,
            format!("s3 client {client_name:?} has an extra bucket with empty section name")
        );
        ensure!(
            !buckets.extra.contains_key(DEFAULT_BUCKET_SECTION),
            AppErr::InvalidConfig,
            format!(
                "s3 client {client_name:?} extra bucket can't be named {DEFAULT_BUCKET_SECTION:?}, it's the default bucket's section name"
            )
        );
        let is_section =
            |name: &str| name == DEFAULT_BUCKET_SECTION || buckets.extra.contains_key(name);
        // 实际的 bucket 名 → 段名
        let mut sections_by_name = BTreeMap::new();
        for (section, bucket_config) in
            default_and_extra(DEFAULT_BUCKET_SECTION, &buckets.default, &buckets.extra)
        {
            let bucket_name = bucket_config.bucket_name(section);
            ensure!(
                !bucket_name.is_empty(),
                AppErr::InvalidConfig,
                format!("s3 client {client_name:?} bucket {section:?} has empty name")
            );
            let max_expires_in = bucket_config.presign_max_expires_in;
            ensure!(
                (PRESIGN_MIN_EXPIRES_IN..=PRESIGN_MAX_EXPIRES_IN).contains(&max_expires_in),
                AppErr::InvalidConfig,
                format!(
                    "s3 client {client_name:?} bucket {section:?} presign_max_expires_in must be between 1s and 7d: {max_expires_in:?}"
                )
            );
            if let Some(name) = &bucket_config.name {
                ensure!(
                    name == section || !is_section(name),
                    AppErr::InvalidConfig,
                    format!(
                        "s3 client {client_name:?} bucket {section:?} has name {name:?}, which is another bucket's section name"
                    )
                );
            }
            if let Some(first_section) = sections_by_name.insert(bucket_name, section) {
                bail!(
                    AppErr::InvalidConfig,
                    format!(
                        "s3 client {client_name:?} buckets {first_section:?} and {section:?} both use bucket {bucket_name:?}"
                    )
                );
            }
        }
    }
    Ok(())
}

/// 默认的一项（名字是 `default_name`）在最前面，后面是 `extra` 里的，都带上名字。客户端、客户端下的 bucket 都是
/// 「一个默认的 + 按名字的 extra」，检查配置时这样一起遍历
fn default_and_extra<'a, T>(
    default_name: &'a str,
    default: &'a T,
    extra: &'a BTreeMap<String, T>,
) -> impl Iterator<Item = (&'a str, &'a T)> {
    std::iter::once((default_name, default))
        .chain(extra.iter().map(|(name, value)| (name.as_str(), value)))
}

/// bucket 在不在：在就返回；不存在（HeadBucket 的 NotFound）时客户端开了 `manage_buckets` 就建，没开报错；其他错误直接报错
async fn ensure_bucket(client_name: &str, client: &AwsS3Client, bucket: &str) -> Result<()> {
    let Err(e) = client.head_bucket().bucket(bucket).send().await else {
        return Ok(());
    };
    if !e.as_service_error().is_some_and(|e| e.is_not_found()) {
        bail!(from: sdk_error("head_bucket", bucket, "", e));
    }
    ensure!(
        client.manage_buckets(),
        from: bucket_not_found(client_name, bucket)
    );
    let mut request = client.create_bucket().bucket(bucket);
    if let Some(location) = client.location_constraint() {
        request = request.create_bucket_configuration(
            CreateBucketConfiguration::builder()
                .location_constraint(location)
                .build(),
        );
    }
    match request.send().await {
        Ok(_) => {
            tracing::info!("S3 bucket created [client={client_name}, bucket={bucket}]");
            Ok(())
        }
        // 同时启动的另一个实例先建好了
        Err(e)
            if e.as_service_error()
                .is_some_and(|e| e.is_bucket_already_owned_by_you()) =>
        {
            Ok(())
        }
        Err(e) => Err(bucket_create_failed(client_name, bucket, e)),
    }
}

#[cfg(test)]
mod tests {
    use hygiea_core::app::{Component, Name, ResourceId};

    use std::time::Duration;

    use super::*;
    use crate::S3BucketsConfig;

    /// 默认 bucket 是默认值（实际的名字是 default），`extra` 是其他 bucket
    fn client_with(extra: &[(&str, S3BucketConfig)]) -> S3ClientConfig {
        S3ClientConfig {
            buckets: S3BucketsConfig {
                default: S3BucketConfig::default(),
                extra: extra
                    .iter()
                    .map(|(name, config)| (name.to_string(), config.clone()))
                    .collect(),
            },
            ..S3ClientConfig::default()
        }
    }

    /// 按组件名声明 AwsS3Store
    #[test]
    fn provides_named_store() {
        let config = S3Config {
            default: S3ClientConfig::default(),
            extra: BTreeMap::new(),
        };
        let component = AwsS3Component::build(Name::from("s3"), config);
        assert_eq!(
            component.provides(),
            vec![ResourceId::named::<AwsS3Store>("s3")]
        );
    }

    fn named(name: &str) -> S3BucketConfig {
        S3BucketConfig {
            name: Some(name.into()),
            ..S3BucketConfig::default()
        }
    }

    /// 预签名的最长有效期不在 1 秒到 7 天之间：报错；7 天（不写时的值）照常通过
    #[test]
    fn presign_max_expires_in_out_of_range_rejected() {
        for max_expires_in in [
            Duration::ZERO,
            PRESIGN_MAX_EXPIRES_IN + Duration::from_secs(1),
        ] {
            let config = S3BucketConfig {
                presign_max_expires_in: max_expires_in,
                ..S3BucketConfig::default()
            };
            let err = check(&[("a", config)]).unwrap_err();
            assert!(err.is(AppErr::InvalidConfig), "{max_expires_in:?}: {err:#}");
        }
        check(&[("a", S3BucketConfig::default())]).unwrap();
    }

    /// 只有默认客户端、它下面的其他 bucket 是 `buckets`，检查配置；同样的 bucket 放在其他客户端下也检查一遍，结果要一样
    fn check(buckets: &[(&str, S3BucketConfig)]) -> Result<()> {
        let default_only = check_config(&S3Config {
            default: client_with(buckets),
            extra: BTreeMap::new(),
        });
        let in_extra = check_config(&S3Config {
            default: S3ClientConfig::default(),
            extra: BTreeMap::from([("a".into(), client_with(buckets))]),
        });
        assert_eq!(default_only.is_ok(), in_extra.is_ok(), "{buckets:?}");
        default_only
    }

    /// 其他客户端的名字是空的、叫 `default`：报错
    #[test]
    fn extra_client_name_reserved() {
        for name in ["", DEFAULT_CLIENT_SECTION] {
            let config = S3Config {
                default: S3ClientConfig::default(),
                extra: BTreeMap::from([(name.into(), S3ClientConfig::default())]),
            };
            let err = check_config(&config).unwrap_err();
            assert!(err.is(AppErr::InvalidConfig), "{name:?}: {err:#}");
        }
    }

    /// 实际的 bucket 名是空的（段名空、name 写了空的）：报错
    #[test]
    fn empty_bucket_rejected() {
        for buckets in [[("", S3BucketConfig::default())], [("files", named(""))]] {
            let err = check(&buckets).unwrap_err();
            assert!(err.is(AppErr::InvalidConfig), "{buckets:?}: {err:#}");
        }
    }

    /// 同一个客户端下实际的 bucket 名重复（两个段写了同一个 name、name 和别的段名一样）：报错；
    /// name 和别的段的段名一样，那个段也写了别的 name、实际的名字不重复：也报错；
    /// 不同的段用不同的名字照常通过，name 和自己的段名一样也没关系
    #[test]
    fn duplicate_bucket_name_rejected() {
        for buckets in [
            [("a", named("real")), ("b", named("real"))],
            [("a", named("b")), ("b", S3BucketConfig::default())],
            [("a", named("b")), ("b", named("c"))],
        ] {
            let err = check(&buckets).unwrap_err();
            assert!(err.is(AppErr::InvalidConfig), "{buckets:?}: {err:#}");
        }
        check(&[("a", named("a")), ("b", named("real"))]).unwrap();
    }

    /// 默认 bucket 的段名是 default：其他 bucket 的段名是空的（写了 name 也不行）、叫 default、name 写成 default
    /// （和默认 bucket 的段名一样）都报错；默认 bucket 的 name 和其他 bucket 的段名一样也报错
    #[test]
    fn default_bucket_section_reserved() {
        for buckets in [
            [("", named("real"))],
            [(DEFAULT_BUCKET_SECTION, S3BucketConfig::default())],
            [("a", named(DEFAULT_BUCKET_SECTION))],
        ] {
            let err = check(&buckets).unwrap_err();
            assert!(err.is(AppErr::InvalidConfig), "{buckets:?}: {err:#}");
        }
        let mut client = client_with(&[("a", S3BucketConfig::default())]);
        client.buckets.default = named("a");
        let err = check_config(&S3Config {
            default: client,
            extra: BTreeMap::new(),
        })
        .unwrap_err();
        assert!(err.is(AppErr::InvalidConfig), "{err:#}");
    }
}
