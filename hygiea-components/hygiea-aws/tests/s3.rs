//! `AwsS3Store` / `AwsS3Component`（`src/s3/`）的集成测试。每个客户端都要有默认 bucket，启动时一定会访问服务，
//! 所以都要连真实服务，分两层（见仓库根目录 AGENTS.md 的测试分层），经 Registry 启动的测试也在 container 层：
//!
//! - `#[container]`：用代码启动 RustFS 容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）
//!   ```sh
//!   cargo test -p hygiea-aws --features s3,s3-bucket-admin --test s3 -- --ignored ::container::
//!   ```
//! - `#[live]`：连自己的 S3 兼容服务（AWS S3、Cloudflare R2 等）。凭证要能管理 bucket（R2 的 Admin Read & Write）：
//!   每个测试建一个 `hygiea-test-<时间戳>` 的 bucket，测完清空删掉
//!   ```sh
//!   export S3_ENDPOINT_URL=https://<ACCOUNT_ID>.r2.cloudflarestorage.com  # 连 AWS 时不设
//!   export S3_REGION=auto  # R2 是 auto，RustFS、MinIO 随便填，连 AWS 时设成真实的区域
//!   export S3_ACCESS_KEY_ID=...
//!   export S3_SECRET_ACCESS_KEY=...
//!   export S3_FORCE_PATH_STYLE=false  # MinIO / RustFS 设 true
//!
//!   cargo test -p hygiea-aws --features s3,s3-bucket-admin --test s3 -- --ignored ::live::
//!   ```

use std::collections::BTreeMap;
use std::io::Write;

use hygiea_aws::aws_sdk_s3;
use hygiea_aws::{
    AwsS3Store, Bytes, IndexSet, PresignGetOptions, PutDecision, PutFile, PutKey, PutOutcome,
    S3BucketConfig, S3BucketHandle, S3BucketName, S3BucketsConfig, S3ClientConfig, S3Config,
    StaticCredentials,
};
use hygiea_core::app::ConfigResource;
use hygiea_test::{container, live};

/// 只有默认的客户端
fn s3_config(client: S3ClientConfig) -> S3Config {
    S3Config {
        default: client,
        extra: BTreeMap::new(),
    }
}

/// 默认 bucket 的实际名字是 `name`、没有其他 bucket
fn default_bucket_named(name: &str) -> S3BucketsConfig {
    S3BucketsConfig {
        default: S3BucketConfig {
            name: Some(name.to_owned()),
            ..S3BucketConfig::default()
        },
        extra: BTreeMap::new(),
    }
}

/// 按客户端配置自己建一个 SDK 客户端，不经过 hygiea：测试里直接调 SDK（按完整 key 读、在前缀外面造数据、看 bucket 在不在）用
fn sdk_client(config: &S3ClientConfig) -> aws_sdk_s3::Client {
    use aws_sdk_s3::config::{BehaviorVersion, Builder, Credentials, Region};

    let credentials = config.credentials.as_ref().unwrap();
    let mut builder = Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(
            config.region.as_ref().unwrap().as_str().to_owned(),
        ))
        .credentials_provider(Credentials::new(
            &credentials.access_key_id,
            &credentials.secret_access_key,
            credentials.session_token.clone(),
            None,
            "hygiea-test",
        ))
        .force_path_style(config.force_path_style);
    if let Some(url) = &config.endpoint_url {
        builder = builder.endpoint_url(url);
    }
    aws_sdk_s3::Client::from_conf(builder.build())
}

/// `echo -n hello | shasum -a 256`
const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

/// 同一个 sha256 的 base64
const HELLO_SHA256_BASE64: &str = "LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=";

/// PNG 文件头，按内容认得出来
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

/// 测试上传用的 key 前缀
const KEY_PREFIX: &str = "files";

/// 上传到 [`KEY_PREFIX`] 下，不看摘要直接传，返回 key（相对句柄）
async fn put_file(bucket: &impl S3BucketHandle, file: PutFile) -> hygiea_core::Result<String> {
    put_file_at(bucket, KEY_PREFIX, file).await
}

/// 上传到 `key_prefix` 下，不看摘要直接传，返回 key（相对句柄）
async fn put_file_at(
    bucket: &impl S3BucketHandle,
    key_prefix: &str,
    file: PutFile,
) -> hygiea_core::Result<String> {
    let outcome = bucket
        .put(key_prefix, PutKey::Content, file, async |_| {
            Ok(PutDecision::<()>::Put)
        })
        .await?;
    let PutOutcome::Put { key, .. } = outcome else {
        panic!("not uploaded: {outcome:?}");
    };
    Ok(key)
}

/// 带扩展名的临时文件，内容是 `content`
fn temp_file(content: &[u8], suffix: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
    file.write_all(content).unwrap();
    file
}

/// 内存里的内容，带上扩展名
fn bytes(bytes: Bytes, file_ext: Option<&str>) -> PutFile {
    PutFile::Bytes {
        bytes,
        file_ext: file_ext.map(str::to_owned),
    }
}

/// 在 `hygiea-test` 前缀下的句柄上传、读回、看元数据、删除，删完确认读不到了
/// 默认客户端的默认 bucket 的实际名字要是 `bucket_name`
async fn put_get_head_delete(store: &AwsS3Store, sdk: &aws_sdk_s3::Client, bucket_name: &str) {
    let bucket = store
        .default_client_default_bucket()
        .with_prefix("hygiea-test")
        .unwrap();
    let key = put_file(
        &bucket,
        bytes(Bytes::from_static(b"hello hygiea"), Some("TXT")),
    )
    .await
    .unwrap();
    // `echo -n 'hello hygiea' | shasum -a 256`，认不出来的用给的扩展名，转成小写
    assert_eq!(
        key,
        "files/c27ea10af2584d3208dfca3aa121d8faddd02dd9d388c822b0d668c7007fec71.txt"
    );

    let body = bucket
        .get(&key)
        .await
        .unwrap()
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(&body[..], b"hello hygiea");
    let (meta, sha256_base64) = bucket.head_with_sha256(&key).await.unwrap().unwrap();
    assert_eq!(meta.size, Some(12));
    assert_eq!(meta.content_type.as_deref(), Some("text/plain"));
    assert!(meta.etag.is_some());
    // `printf 'hello hygiea' | shasum -a 256 | cut -d' ' -f1 | xxd -r -p | base64`
    assert_eq!(
        sha256_base64,
        "wn6hCvJYTTII38o6oSHY+t3QLdnTiMgisNZoxwB/7HE="
    );

    // 前缀加在 key 前面：直接用 SDK 按完整 key 也读得到，不写 Content-Disposition
    let output = sdk
        .head_object()
        .bucket(bucket_name)
        .key(format!("hygiea-test/{key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(output.content_disposition(), None);

    bucket.delete(&key).await.unwrap();
    assert!(bucket.get(&key).await.unwrap().is_none());
    assert!(bucket.head(&key).await.unwrap().is_none());
    // 删不存在的对象也算成功
    bucket.delete(&key).await.unwrap();
}

/// 预签名 GET 时覆盖的响应头，和 [`assert_overridden`] 对应
fn overrides() -> PresignGetOptions {
    PresignGetOptions {
        response_content_type: Some("audio/mpeg".into()),
        response_cache_control: Some("max-age=60".into()),
        ..PresignGetOptions::attachment("さくら 1.mp3")
    }
}

/// 响应头是 [`overrides`] 里签进 URL 的值
fn assert_overridden(response: &reqwest::Response) {
    assert!(response.status().is_success(), "{response:?}");
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(header("content-type").as_deref(), Some("audio/mpeg"));
    assert_eq!(
        header("content-disposition").as_deref(),
        Some(
            r#"attachment; filename="___ 1.mp3"; filename*=UTF-8''%E3%81%95%E3%81%8F%E3%82%89%201.mp3"#
        )
    );
    assert_eq!(header("cache-control").as_deref(), Some("max-age=60"));
}

/// 用代码启动的 RustFS（兼容 S3 的对象存储）
#[container]
mod rustfs {
    use std::time::Duration;

    use hygiea_aws::{AwsS3Component, ByteStream, S3Err};
    use hygiea_core::app::{Registry, Resources};
    use hygiea_test::container::{RunningS3, S3Spec};

    use super::*;

    async fn start() -> RunningS3 {
        S3Spec::new().start().await
    }

    /// 自建服务的写法：path-style，region 随便填
    fn client_config(
        server: &RunningS3,
        secret_key: &str,
        buckets: S3BucketsConfig,
    ) -> S3ClientConfig {
        S3ClientConfig {
            region: Some("us-east-1".into()),
            endpoint_url: Some(server.endpoint()),
            credentials: Some(StaticCredentials {
                access_key_id: server.access_key().into(),
                secret_access_key: secret_key.into(),
                session_token: None,
            }),
            force_path_style: true,
            manage_buckets: true,
            buckets,
            ..S3ClientConfig::default()
        }
    }

    async fn store(server: &RunningS3, buckets: S3BucketsConfig) -> AwsS3Store {
        let config = s3_config(client_config(server, server.secret_key(), buckets));
        AwsS3Store::from_config(&config, &Default::default())
            .await
            .unwrap()
    }

    /// bucket 不存在、客户端没开 manage_buckets（不让建）：启动失败
    #[tokio::test]
    async fn missing_bucket_fails_startup() {
        let server = start().await;
        let config = s3_config(S3ClientConfig {
            manage_buckets: false,
            ..client_config(&server, server.secret_key(), default_bucket_named("absent"))
        });
        let err = AwsS3Store::from_config(&config, &Default::default())
            .await
            .unwrap_err();
        assert!(err.is(S3Err::BucketNotFound), "{err:#}");
    }

    /// 客户端开了 manage_buckets：bucket 不存在就建；再启动一次（已经存在）照常启动
    #[tokio::test]
    async fn manage_buckets_creates_missing_bucket() {
        let server = start().await;
        store(&server, default_bucket_named("created")).await;
        let sdk = sdk_client(&client_config(
            &server,
            server.secret_key(),
            S3BucketsConfig::default(),
        ));
        sdk.head_bucket().bucket("created").send().await.unwrap();
        store(&server, default_bucket_named("created")).await;
    }

    /// 默认 bucket 的段名是 default，实际的 bucket 名按 name：取到的句柄请求发到实际的 bucket（启动时按实际的名字建）。
    /// 客户端下的其他 bucket 同样按实际的名字建；默认客户端下的按段名取（default_client_buckets），结果里没有默认 bucket；
    /// 段名可以用字符串，也可以用自己实现了 S3BucketName 的类型；没配的段名（包括 default）报 BucketNotConfigured
    #[tokio::test]
    async fn bucket_name_differs_from_section() {
        #[derive(PartialEq, Eq, Hash)]
        enum TestBucket {
            Files,
        }

        impl S3BucketName for TestBucket {
            fn bucket_name(&self) -> &str {
                match self {
                    Self::Files => "files",
                }
            }
        }

        let server = start().await;
        let mut buckets = default_bucket_named("real");
        buckets.extra.insert(
            "files".into(),
            S3BucketConfig {
                name: Some("real-files".into()),
                ..S3BucketConfig::default()
            },
        );
        let store = store(&server, buckets).await;
        let sdk = sdk_client(&client_config(
            &server,
            server.secret_key(),
            S3BucketsConfig::default(),
        ));
        let key = put_file(
            &store.default_client_default_bucket(),
            bytes(Bytes::from_static(b"a"), Some("txt")),
        )
        .await
        .unwrap();
        sdk.head_object()
            .bucket("real")
            .key(&key)
            .send()
            .await
            .unwrap();
        sdk.head_bucket().bucket("real-files").send().await.unwrap();

        let buckets = store
            .default_client_buckets(&IndexSet::from([TestBucket::Files]))
            .unwrap();
        assert_eq!(
            buckets.keys().map(String::as_str).collect::<Vec<_>>(),
            ["files"]
        );
        let files_key = put_file(
            &buckets["files"],
            bytes(Bytes::from_static(b"b"), Some("txt")),
        )
        .await
        .unwrap();
        sdk.head_object()
            .bucket("real-files")
            .key(&files_key)
            .send()
            .await
            .unwrap();
        let buckets = store
            .default_client_buckets(&IndexSet::from(["files".to_owned()]))
            .unwrap();
        assert_eq!(
            buckets.keys().map(String::as_str).collect::<Vec<_>>(),
            ["files"]
        );
        for name in ["real-files", "default"] {
            let err = store
                .default_client_buckets(&IndexSet::from([name]))
                .unwrap_err();
            assert!(err.is(S3Err::BucketNotConfigured), "{name}: {err:#}");
        }
    }

    /// 客户端开了 manage_buckets 但凭证没有建 bucket 的权限：启动失败，报 BucketCreateFailed。
    /// 用 add_user 建一个只能读写、列对象的用户，HeadBucket 拿到"不存在"，CreateBucket 被拒
    #[tokio::test]
    async fn create_failure_fails_startup() {
        let server = start().await;
        server
            .add_user(
                "limited",
                "limited-secret",
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject","s3:ListBucket"],"Resource":["arn:aws:s3:::*"]}]}"#,
            )
            .await;
        let client = S3ClientConfig {
            credentials: Some(StaticCredentials {
                access_key_id: "limited".into(),
                secret_access_key: "limited-secret".into(),
                session_token: None,
            }),
            ..client_config(&server, server.secret_key(), default_bucket_named("absent"))
        };
        let err = AwsS3Store::from_config(&s3_config(client), &Default::default())
            .await
            .unwrap_err();
        assert!(err.is(S3Err::BucketCreateFailed), "{err:#}");
    }

    /// 密钥不对：启动时 HeadBucket 被服务端拒绝，报服务端错误，不去建 bucket
    #[tokio::test]
    async fn wrong_secret_fails_startup() {
        let server = start().await;
        let config = s3_config(client_config(
            &server,
            "wrong-secret",
            default_bucket_named("any"),
        ));
        let err = AwsS3Store::from_config(&config, &Default::default())
            .await
            .unwrap_err();
        assert!(err.is(S3Err::ServiceError), "{err:#}");
    }

    #[tokio::test]
    async fn put_get_head_delete_object() {
        let server = start().await;
        let store = store(&server, default_bucket_named("test")).await;
        let sdk = sdk_client(&client_config(
            &server,
            server.secret_key(),
            S3BucketsConfig::default(),
        ));
        put_get_head_delete(&store, &sdk, "test").await;
    }

    /// 带前缀的句柄：读写的 key 都加上前缀（补了结尾的 `/`），`put` 返回的 key 相对前缀；
    /// 不带前缀的句柄、直接用 SDK 按完整 key 能读到，前缀外面的读不到
    #[tokio::test]
    async fn with_prefix_applied() {
        let server = start().await;
        let store = store(&server, default_bucket_named("test")).await;
        let sdk = sdk_client(&client_config(
            &server,
            server.secret_key(),
            S3BucketsConfig::default(),
        ));
        let bucket = store.default_client_default_bucket();
        let prefixed = bucket.with_prefix("root").unwrap();

        let key = put_file(&prefixed, bytes(Bytes::from_static(b"a"), Some("txt")))
            .await
            .unwrap();
        assert!(key.starts_with("files/"), "{key}");
        assert!(prefixed.get(&key).await.unwrap().is_some());
        assert!(bucket.get(&format!("root/{key}")).await.unwrap().is_some());
        sdk.head_object()
            .bucket("test")
            .key(format!("root/{key}"))
            .send()
            .await
            .unwrap();
        sdk.put_object()
            .bucket("test")
            .key("b.txt")
            .body(Bytes::from_static(b"b").into())
            .send()
            .await
            .unwrap();
        assert!(prefixed.get("b.txt").await.unwrap().is_none());

        // 上传时没带 sha256 的：get、head 读得到，带 sha256 的读法报 MissingSha256
        assert!(bucket.head("b.txt").await.unwrap().is_some());
        assert!(bucket.get("b.txt").await.unwrap().is_some());
        let err = bucket.head_with_sha256("b.txt").await.unwrap_err();
        assert!(err.is(S3Err::MissingSha256), "{err:#}");
        let err = bucket.get_with_sha256("b.txt").await.unwrap_err();
        assert!(err.is(S3Err::MissingSha256), "{err:#}");
    }

    /// 默认的客户端和其他客户端（各连一个服务）下可以有同名的 bucket，互不干扰；没配的其他客户端报 ClientNotConfigured。
    /// 按名字取其他客户端的默认 bucket（client_default_bucket）、按段名取其他 bucket（client_buckets）；
    /// 一次取几个其他客户端的默认 bucket（clients_default_buckets）、几个 bucket（clients_buckets），结果里都没有默认客户端、默认 bucket
    #[tokio::test]
    async fn same_bucket_name_under_two_clients() {
        let (first_server, second_server) = tokio::join!(start(), start());
        let client = |server: &RunningS3| {
            client_config(server, server.secret_key(), default_bucket_named("audio"))
        };
        // second 还多一个其他 bucket images
        let mut second_client = client(&second_server);
        second_client.buckets.extra.insert(
            "images".into(),
            S3BucketConfig {
                name: Some("second-images".into()),
                ..S3BucketConfig::default()
            },
        );
        let config = S3Config {
            default: client(&first_server),
            extra: BTreeMap::from([("second".to_string(), second_client)]),
        };
        let store = AwsS3Store::from_config(&config, &Default::default())
            .await
            .unwrap();
        let first = store.default_client_default_bucket();
        let second = store.client_default_bucket("second").unwrap();

        let key = put_file(&first, bytes(Bytes::from_static(b"a"), Some("txt")))
            .await
            .unwrap();
        assert!(first.get(&key).await.unwrap().is_some());
        assert!(second.get(&key).await.unwrap().is_none());
        let err = store.client_default_bucket("third").unwrap_err();
        assert!(err.is(S3Err::ClientNotConfigured), "{err:#}");
        // 其他客户端上按段名取：按给的顺序，没有默认 bucket；没配的段名、客户端报错
        let images = IndexSet::from(["images".to_owned()]);
        let buckets = store.client_buckets("second", &images).unwrap();
        assert_eq!(
            buckets.keys().map(String::as_str).collect::<Vec<_>>(),
            ["images"]
        );
        let image_key = put_file(
            &buckets["images"],
            bytes(Bytes::from_static(b"i"), Some("txt")),
        )
        .await
        .unwrap();
        assert!(buckets["images"].get(&image_key).await.unwrap().is_some());
        for name in ["videos", "default"] {
            let err = store
                .client_buckets("second", &IndexSet::from([name]))
                .unwrap_err();
            assert!(err.is(S3Err::BucketNotConfigured), "{name}: {err:#}");
        }
        let err = store.client_buckets("third", &images).unwrap_err();
        assert!(err.is(S3Err::ClientNotConfigured), "{err:#}");

        // 几个客户端上按段名取：外层是客户端，内层是 bucket，都按给的顺序；每个客户端下都要配了这些段名
        let names = |names: &[&str]| {
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<IndexSet<_>>()
        };
        let clients = store.clients_buckets(&names(&["second"]), &images).unwrap();
        assert_eq!(
            clients.keys().map(String::as_str).collect::<Vec<_>>(),
            ["second"]
        );
        assert_eq!(
            clients["second"]
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["images"]
        );
        assert!(
            clients["second"]["images"]
                .get(&image_key)
                .await
                .unwrap()
                .is_some()
        );

        // 一次取几个客户端的默认 bucket：按给的顺序，句柄就是各自客户端上的
        let buckets = store.clients_default_buckets(&names(&["second"])).unwrap();
        assert_eq!(
            buckets.keys().map(String::as_str).collect::<Vec<_>>(),
            ["second"]
        );
        assert!(buckets["second"].get(&key).await.unwrap().is_none());
        assert!(
            store
                .clients_default_buckets(&IndexSet::new())
                .unwrap()
                .is_empty()
        );
        // 没配的客户端（包括写成 default）：两个方法都报错
        for extra in [&["third"][..], &["default"][..]] {
            let err = store.clients_default_buckets(&names(extra)).unwrap_err();
            assert!(err.is(S3Err::ClientNotConfigured), "{extra:?}: {err:#}");
            let err = store.clients_buckets(&names(extra), &images).unwrap_err();
            assert!(err.is(S3Err::ClientNotConfigured), "{extra:?}: {err:#}");
        }
    }

    /// 字节流读：元数据和内容都对；当 AsyncRead 一块块读到写入端；对象不存在返回 None
    #[tokio::test]
    async fn get_reads_body_as_stream() {
        let server = start().await;
        let store = store(&server, default_bucket_named("test")).await;
        let bucket = store.default_client_default_bucket();
        let content: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        let key = put_file(&bucket, bytes(Bytes::from(content.clone()), Some("bin")))
            .await
            .unwrap();

        let object = bucket.get(&key).await.unwrap().unwrap();
        assert_eq!(object.meta.size, Some(content.len() as i64));
        assert_eq!(
            object.meta.content_type.as_deref(),
            Some("application/octet-stream")
        );
        let mut reader = object.body.into_async_read();
        let mut written = Vec::new();
        tokio::io::copy(&mut reader, &mut written).await.unwrap();
        assert_eq!(written, content);

        assert!(bucket.get("absent.bin").await.unwrap().is_none());
    }

    /// 上传本地文件
    #[tokio::test]
    async fn put_uploads_file() {
        let server = start().await;
        let store = store(&server, default_bucket_named("test")).await;
        let bucket = store.default_client_default_bucket();
        let content: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        let file = temp_file(&content, ".bin");

        let key = put_file(&bucket, PutFile::Path(file.path().to_owned()))
            .await
            .unwrap();
        assert_eq!(
            bucket
                .get(&key)
                .await
                .unwrap()
                .unwrap()
                .bytes()
                .await
                .unwrap(),
            content
        );

        let err = put_file(&bucket, PutFile::Path("/no/such/file.bin".into()))
            .await
            .unwrap_err();
        assert!(err.is(S3Err::ReadFileFailed), "{err:#}");
    }

    /// put 按 decide 的决定：Skip 不上传、带回值；decide 报错不上传、原样返回它的错误；
    /// Put 传到 前缀 + sha256 + 扩展名，返回 key 和摘要（Content-Type、扩展名在 decide 之前就定好）。
    /// 认得出来的用认出的扩展名，给的对不上报 ContentTypeMismatch；认不出来的用给的，没给的 key 不带扩展名、类型是 octet-stream；
    /// 扩展名不合规则的不调 decide，报 InvalidFileExt
    #[tokio::test]
    async fn put_follows_decision() {
        let server = start().await;
        let store = store(&server, default_bucket_named("test")).await;
        let bucket = store.default_client_default_bucket();
        let hello = |file_ext| bytes(Bytes::from_static(b"hello"), file_ext);
        let hello_key = format!("files/{HELLO_SHA256}.txt");

        let outcome = bucket
            .put(
                KEY_PREFIX,
                PutKey::Content,
                hello(Some("TXT")),
                async |digest| {
                    assert_eq!(digest.sha256_hex(), HELLO_SHA256);
                    assert_eq!(digest.size(), 5);
                    assert_eq!(digest.content_type(), "text/plain");
                    assert_eq!(digest.file_ext(), Some("txt"));
                    Ok(PutDecision::Skip(7))
                },
            )
            .await
            .unwrap();
        assert!(matches!(outcome, PutOutcome::Skipped(7)), "{outcome:?}");

        let err = bucket
            .put(
                KEY_PREFIX,
                PutKey::Content,
                hello(Some("txt")),
                async |_| Err::<PutDecision<()>, _>(hygiea_core::impossible!("decide failed")),
            )
            .await
            .unwrap_err();
        assert!(err.is(hygiea_core::BaseErr::Impossible), "{err:#}");
        assert!(bucket.get(&hello_key).await.unwrap().is_none());

        let outcome = bucket
            .put(
                KEY_PREFIX,
                PutKey::Content,
                hello(Some("txt")),
                async |_| Ok(PutDecision::<()>::Put),
            )
            .await
            .unwrap();
        let PutOutcome::Put { key, digest } = outcome else {
            panic!("not uploaded: {outcome:?}");
        };
        assert_eq!(key, hello_key);
        assert_eq!(digest.sha256_hex(), HELLO_SHA256);
        let meta = bucket.head(&key).await.unwrap().unwrap();
        assert_eq!(meta.content_type.as_deref(), Some("text/plain"));

        // 认不出来、也没给扩展名：key 不带扩展名，类型是 octet-stream
        let key = put_file(&bucket, hello(None)).await.unwrap();
        assert_eq!(key, format!("files/{HELLO_SHA256}"));
        let meta = bucket.head(&key).await.unwrap().unwrap();
        assert_eq!(
            meta.content_type.as_deref(),
            Some("application/octet-stream")
        );

        // 认得出来的用认出的扩展名，不管给没给
        let png = |file_ext| bytes(Bytes::from_static(PNG), file_ext);
        let png_key = put_file(&bucket, png(None)).await.unwrap();
        assert!(png_key.ends_with(".png"), "{png_key}");
        assert_eq!(put_file(&bucket, png(Some("PNG"))).await.unwrap(), png_key);
        let meta = bucket.head(&png_key).await.unwrap().unwrap();
        assert_eq!(meta.content_type.as_deref(), Some("image/png"));
        let err = put_file(&bucket, png(Some("mp3"))).await.unwrap_err();
        assert!(err.is(S3Err::ContentTypeMismatch), "{err:#}");

        for file_ext in ["", ".txt", "t-xt"] {
            let err = bucket
                .put(
                    KEY_PREFIX,
                    PutKey::Content,
                    hello(Some(file_ext)),
                    async |_| -> hygiea_core::Result<PutDecision<()>> {
                        panic!("decide called for invalid file ext")
                    },
                )
                .await
                .unwrap_err();
            assert!(err.is(S3Err::InvalidFileExt), "{file_ext:?}: {err:#}");
        }

        let err = put_file_at(&bucket, "/files", hello(None))
            .await
            .unwrap_err();
        assert!(err.is(S3Err::InvalidKey), "{err:#}");
        let err = bucket.with_prefix("a//b").unwrap_err();
        assert!(err.is(S3Err::InvalidKey), "{err:#}");
    }

    /// put_object 按给的 key、Content-Type 写进去，带上给的 sha256、大小：内存里的内容、另一个对象读出来的流都能传；
    /// sha256 和内容对不上的服务端拒绝写入（ServiceError）
    #[tokio::test]
    async fn put_object_with_given_key() {
        let server = start().await;
        let store = store(&server, default_bucket_named("test")).await;
        let bucket = store.default_client_default_bucket();
        let hello = || Bytes::from_static(b"hello");

        bucket
            .put_object(
                "copied/hello",
                hello(),
                HELLO_SHA256_BASE64,
                5,
                "text/plain",
            )
            .await
            .unwrap();
        let body = bucket
            .get("copied/hello")
            .await
            .unwrap()
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(&body[..], b"hello");
        let meta = bucket.head("copied/hello").await.unwrap().unwrap();
        assert_eq!(meta.content_type.as_deref(), Some("text/plain"));

        // 读出来的流原样写到另一个 key，不进内存
        let source = bucket.get("copied/hello").await.unwrap().unwrap();
        bucket
            .put_object(
                "copied/hello-stream",
                source.body,
                HELLO_SHA256_BASE64,
                5,
                "text/plain",
            )
            .await
            .unwrap();
        let (object, sha256_base64) = bucket
            .get_with_sha256("copied/hello-stream")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sha256_base64, HELLO_SHA256_BASE64);
        assert_eq!(&object.bytes().await.unwrap()[..], b"hello");

        // `printf HELLO | shasum -a 256 | cut -d' ' -f1 | xxd -r -p | base64`：别的内容的 sha256
        let other = "NzPNl3/46xi5hzV+Is7Zn0YJfzHssjnoeK5jdg6D5NU=";
        let err = bucket
            .put_object("copied/mismatch", hello(), other, 5, "text/plain")
            .await
            .unwrap_err();
        assert!(err.is(S3Err::ServiceError), "{err:#}");
        assert!(bucket.head("copied/mismatch").await.unwrap().is_none());
    }

    /// 流按带的 sha256、字节数、Content-Type、扩展名上传：key 用带的 sha256 拼；sha256 和内容对不上的服务端拒绝写入
    #[tokio::test]
    async fn put_uploads_stream() {
        let server = start().await;
        let store = store(&server, default_bucket_named("test")).await;
        let bucket = store.default_client_default_bucket();
        let stream = |sha256: [u8; 32]| PutFile::Stream {
            body: ByteStream::from_static(b"hello"),
            sha256,
            size: 5,
            content_type: "text/plain".into(),
            file_ext: Some("txt".into()),
        };
        let hello_sha256: [u8; 32] = (0..32)
            .map(|i| u8::from_str_radix(&HELLO_SHA256[i * 2..i * 2 + 2], 16).unwrap())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        let key = put_file(&bucket, stream(hello_sha256)).await.unwrap();
        assert_eq!(key, format!("files/{HELLO_SHA256}.txt"));
        let body = bucket
            .get(&key)
            .await
            .unwrap()
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(&body[..], b"hello");
        let (meta, sha256_base64) = bucket.head_with_sha256(&key).await.unwrap().unwrap();
        assert_eq!(meta.content_type.as_deref(), Some("text/plain"));
        assert_eq!(sha256_base64, HELLO_SHA256_BASE64);

        let err = put_file(&bucket, stream([0; 32])).await.unwrap_err();
        assert!(err.is(S3Err::ServiceError), "{err:#}");
        let zero_key = format!("files/{}.txt", "00".repeat(32));
        assert!(bucket.head(&zero_key).await.unwrap().is_none());
    }

    /// PutKey::Named：key 是前缀 + 给的名字 + 扩展名（扩展名的定法和 Content 一样），名字一样的覆盖；
    /// 没有扩展名的不带 `.`；名字不合规则的不调 decide，报 InvalidKey
    #[tokio::test]
    async fn put_named_key() {
        let server = start().await;
        let store = store(&server, default_bucket_named("test")).await;
        let bucket = store.default_client_default_bucket();
        let put_named = async |name: &str, file: PutFile| -> hygiea_core::Result<String> {
            let outcome = bucket
                .put(KEY_PREFIX, PutKey::Named(name.into()), file, async |_| {
                    Ok(PutDecision::<()>::Put)
                })
                .await?;
            let PutOutcome::Put { key, .. } = outcome else {
                panic!("not uploaded: {outcome:?}");
            };
            Ok(key)
        };

        let key = put_named(
            "2026/report",
            bytes(Bytes::from_static(b"one"), Some("TXT")),
        )
        .await
        .unwrap();
        assert_eq!(key, "files/2026/report.txt");
        let key = put_named(
            "2026/report",
            bytes(Bytes::from_static(b"two"), Some("txt")),
        )
        .await
        .unwrap();
        let body = bucket
            .get(&key)
            .await
            .unwrap()
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(&body[..], b"two");

        // 空内容、没有扩展名：key 就是前缀 + 名字，可以存成很多个不同的对象
        for name in ["markers/a/_SUCCESS", "markers/b/_SUCCESS"] {
            let key = put_named(name, bytes(Bytes::new(), None)).await.unwrap();
            assert_eq!(key, format!("files/{name}"));
            let meta = bucket.head(&key).await.unwrap().unwrap();
            assert_eq!(meta.size, Some(0));
        }

        for name in ["", "/report", "a//b"] {
            let err = bucket
                .put(
                    KEY_PREFIX,
                    PutKey::Named(name.into()),
                    bytes(Bytes::from_static(b"x"), None),
                    async |_| -> hygiea_core::Result<PutDecision<()>> {
                        panic!("decide called for invalid key name")
                    },
                )
                .await
                .unwrap_err();
            assert!(err.is(S3Err::InvalidKey), "{name:?}: {err:#}");
        }
    }

    /// 本地文件算完摘要、上传之前被改了（长度不变）：带的 sha256 和服务端收到的内容对不上，拒绝写入
    #[tokio::test]
    async fn put_rejects_file_changed_after_digest() {
        let server = start().await;
        let store = store(&server, default_bucket_named("test")).await;
        let bucket = store.default_client_default_bucket();
        let file = temp_file(b"hello", ".txt");
        let path = file.path().to_owned();

        let err = bucket
            .put(
                KEY_PREFIX,
                PutKey::Content,
                PutFile::Path(path.clone()),
                async |_| {
                    std::fs::write(&path, b"HELLO").unwrap();
                    Ok(PutDecision::<()>::Put)
                },
            )
            .await
            .unwrap_err();
        assert!(err.is(S3Err::ServiceError), "{err:#}");
        let hello_key = format!("files/{HELLO_SHA256}.txt");
        assert!(bucket.get(&hello_key).await.unwrap().is_none());
    }

    /// bucket 不存在时 get 报错，不是 None（None 只表示对象不存在）：启动时建好的 bucket，再用 SDK 删掉
    #[tokio::test]
    async fn get_from_missing_bucket_is_error() {
        let server = start().await;
        let store = store(&server, default_bucket_named("gone")).await;
        let bucket = store.default_client_default_bucket();
        bucket.delete_bucket().await.unwrap();
        let err = bucket.get("a.txt").await.unwrap_err();
        assert!(err.is(S3Err::ServiceError), "{err:#}");
    }

    /// 删 bucket：不空时报错；empty_and_delete_bucket 删光对象再删 bucket
    #[tokio::test]
    async fn delete_bucket_after_emptying() {
        let server = start().await;
        let store = store(&server, default_bucket_named("full")).await;
        let sdk = sdk_client(&client_config(
            &server,
            server.secret_key(),
            S3BucketsConfig::default(),
        ));
        let bucket = store.default_client_default_bucket();
        for content in [b"1", b"2"] {
            put_file(&bucket, bytes(Bytes::from_static(content), Some("txt")))
                .await
                .unwrap();
        }
        sdk.put_object()
            .bucket("full")
            .key("b/3.txt")
            .body(Bytes::from_static(b"x").into())
            .send()
            .await
            .unwrap();

        let err = bucket.delete_bucket().await.unwrap_err();
        assert!(err.is(S3Err::ServiceError), "{err:#}");

        bucket.empty_and_delete_bucket().await.unwrap();
        let err = sdk.head_bucket().bucket("full").send().await.unwrap_err();
        assert!(err.into_service_error().is_not_found());
    }

    /// 客户端没开 manage_buckets：删 bucket 报 BucketAdminDisabled，bucket 还在
    #[tokio::test]
    async fn bucket_admin_needs_manage_buckets() {
        let server = start().await;
        // 先用能管理 bucket 的客户端把 bucket 建好
        store(&server, default_bucket_named("kept")).await;
        let client = S3ClientConfig {
            manage_buckets: false,
            ..client_config(&server, server.secret_key(), default_bucket_named("kept"))
        };
        let sdk = sdk_client(&client);
        let store = AwsS3Store::from_config(&s3_config(client), &Default::default())
            .await
            .unwrap();
        let bucket = store.default_client_default_bucket();
        let err = bucket.delete_bucket().await.unwrap_err();
        assert!(err.is(S3Err::BucketAdminDisabled), "{err:#}");
        let err = bucket.empty_and_delete_bucket().await.unwrap_err();
        assert!(err.is(S3Err::BucketAdminDisabled), "{err:#}");
        sdk.head_bucket().bucket("kept").send().await.unwrap();
    }

    /// 预签名 GET 读回对象；不传有效期用 bucket 配的最长有效期，传的超过它或者不到 1 秒报错
    #[tokio::test]
    async fn presigned_get_reads_object() {
        let server = start().await;
        let mut buckets = default_bucket_named("test");
        buckets.default.presign_max_expires_in = Duration::from_secs(3600);
        let store = store(&server, buckets).await;
        let bucket = store.default_client_default_bucket();
        let http = reqwest::Client::new();

        bucket
            .put_object(
                "presigned.txt",
                Bytes::from_static(b"via presigned"),
                // `printf 'via presigned' | shasum -a 256 | cut -d' ' -f1 | xxd -r -p | base64`
                "GZ+P8Jcw6uuDDQNpOwGSbjEihXOzU9hh4iTTpze+ChI=",
                13,
                "text/plain",
            )
            .await
            .unwrap();

        let get_url = bucket
            .presigned_get_url("presigned.txt", None, PresignGetOptions::default())
            .await
            .unwrap();
        assert!(get_url.contains("X-Amz-Expires=3600"), "{get_url}");
        let body = http
            .get(&get_url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(&body[..], b"via presigned");

        // 签进 URL 的 response-* 覆盖响应头
        let response = http
            .get(
                bucket
                    .presigned_get_url("presigned.txt", Some(Duration::from_secs(600)), overrides())
                    .await
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();
        assert_overridden(&response);

        for expires_in in [Duration::from_secs(3601), Duration::ZERO] {
            let err = bucket
                .presigned_get_url(
                    "presigned.txt",
                    Some(expires_in),
                    PresignGetOptions::default(),
                )
                .await
                .unwrap_err();
            assert!(
                err.is(S3Err::InvalidPresignExpiry),
                "{expires_in:?}: {err:#}"
            );
        }
    }

    /// 本地反向代理：请求原样（Host 头、路径、query 都不改）转给 `upstream`（`host:port`），
    /// 返回代理的地址 `127.0.0.1:<端口>`
    async fn start_proxy(upstream: String) -> String {
        use tokio::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            loop {
                let (mut downstream, _) = listener.accept().await.unwrap();
                let upstream = upstream.clone();
                tokio::spawn(async move {
                    let mut upstream = TcpStream::connect(&upstream).await.unwrap();
                    tokio::io::copy_bidirectional(&mut downstream, &mut upstream)
                        .await
                        .ok();
                });
            }
        });
        addr
    }

    /// 配了 presign_public_origin（代理的地址）：按代理的地址签名，经代理原样转给服务后验签通过，能读到内容；
    /// 服务端照常连服务地址（上传走的是它）
    #[tokio::test]
    async fn presign_public_origin_through_proxy() {
        let server = start().await;
        let upstream = server.endpoint().trim_start_matches("http://").to_owned();
        let proxy = start_proxy(upstream).await;
        let client = S3ClientConfig {
            presign_public_origin: Some(format!("http://{proxy}")),
            ..client_config(&server, server.secret_key(), default_bucket_named("test"))
        };
        let store = AwsS3Store::from_config(&s3_config(client), &Default::default())
            .await
            .unwrap();
        let bucket = store.default_client_default_bucket();
        let key = put_file(&bucket, bytes(Bytes::from_static(b"a"), Some("txt")))
            .await
            .unwrap();

        let url = bucket
            .presigned_get_url(
                &key,
                Some(Duration::from_secs(600)),
                PresignGetOptions::default(),
            )
            .await
            .unwrap();
        assert!(
            url.starts_with(&format!("http://{proxy}/test/{key}?")),
            "{url}"
        );
        let response = reqwest::get(&url).await.unwrap();
        assert!(response.status().is_success(), "{response:?}");
        assert_eq!(&response.bytes().await.unwrap()[..], b"a");
    }

    /// 启动 registry，把 `Resources` 交给 `check`，拿回它的结果。
    /// 启动成功后 `run` 会一直等退出信号，所以放到后台任务里，拿到结果就 abort
    async fn with_resources<T: Send + 'static>(
        registry: Registry,
        check: impl FnOnce(Resources) -> T + Send + 'static,
    ) -> T {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = registry
                .on_ready(move |resources| async move {
                    let _ = tx.send(check(resources));
                    Ok(())
                })
                .run()
                .await;
        });
        let result = rx.await.unwrap();
        task.abort();
        result
    }

    /// 经 Registry 启动，按配置提供 AwsS3Store：配置里的客户端取得到默认 bucket，没有的客户端报 ClientNotConfigured
    #[tokio::test]
    async fn registry_provides_store() {
        let server = start().await;
        let config = S3Config {
            default: client_config(
                &server,
                server.secret_key(),
                default_bucket_named("registry-default"),
            ),
            extra: BTreeMap::from([(
                "r2".to_string(),
                client_config(
                    &server,
                    server.secret_key(),
                    default_bucket_named("registry-r2"),
                ),
            )]),
        };
        let registry = Registry::new().add::<AwsS3Component>(config);
        // 默认的和配了的其他客户端取得到默认 bucket；没配的客户端报 ClientNotConfigured
        let (r2_found, unknown_client) = with_resources(registry, |res| {
            let store = res.get::<AwsS3Store>().unwrap();
            (
                store.client_default_bucket("r2").is_ok(),
                store
                    .client_default_bucket("absent")
                    .unwrap_err()
                    .is(S3Err::ClientNotConfigured),
            )
        })
        .await;
        assert!(r2_found);
        assert!(unknown_client);
    }
}

/// 自己的 S3 兼容服务（比如 Cloudflare R2）。凭证要能管理 bucket（R2 的 Admin Read & Write）：
/// 每个测试建一个 `hygiea-test-<时间戳>` 的 bucket，在里面测，测完清空删掉（中途失败也删）
#[live(env = ["S3_REGION", "S3_ACCESS_KEY_ID", "S3_SECRET_ACCESS_KEY"])]
mod hosted {
    use std::future::Future;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use hygiea_aws::S3Err;
    use hygiea_core::env::{EnvKey, env_get, env_get_opt};

    use super::*;

    /// 用到的环境变量，名字见文件开头
    #[derive(Clone, Copy)]
    enum S3Env {
        EndpointUrl,
        Region,
        ForcePathStyle,
        AccessKeyId,
        SecretAccessKey,
    }

    impl EnvKey for S3Env {
        fn key_name(&self) -> &str {
            match self {
                Self::EndpointUrl => "S3_ENDPOINT_URL",
                Self::Region => "S3_REGION",
                Self::ForcePathStyle => "S3_FORCE_PATH_STYLE",
                Self::AccessKeyId => "S3_ACCESS_KEY_ID",
                Self::SecretAccessKey => "S3_SECRET_ACCESS_KEY",
            }
        }
    }

    /// 连接信息从环境变量读；`manage_buckets` 开了时启动会建不存在的 bucket
    fn config(bucket_name: &str, manage_buckets: bool) -> S3Config {
        s3_config(S3ClientConfig {
            region: Some(env_get(S3Env::Region).unwrap().into()),
            endpoint_url: env_get_opt(S3Env::EndpointUrl),
            credentials: Some(StaticCredentials {
                access_key_id: env_get(S3Env::AccessKeyId).unwrap(),
                secret_access_key: env_get(S3Env::SecretAccessKey).unwrap(),
                session_token: None,
            }),
            force_path_style: env_get_opt(S3Env::ForcePathStyle).is_some_and(|v| v == "true"),
            manage_buckets,
            buckets: default_bucket_named(bucket_name),
            ..S3ClientConfig::default()
        })
    }

    /// 建一个临时 bucket 交给 `test`，结束后（不管成没成功）清空删掉，再照常报 `test` 里的失败
    async fn with_temp_bucket<F, Fut>(test: F)
    where
        F: FnOnce(AwsS3Store, String) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let bucket_name = format!("hygiea-test-{nanos}");
        let name = bucket_name.clone();
        let outcome = tokio::spawn(async move {
            let store = AwsS3Store::from_config(&config(&name, true), &Default::default())
                .await
                .unwrap();
            test(store, name).await;
        })
        .await;
        remove_bucket(&bucket_name).await;
        if let Err(e) = outcome {
            std::panic::resume_unwind(e.into_panic());
        }
    }

    /// bucket 还在就清空删掉（先用不建 bucket 的配置检查，不在就算了），删不掉打出名字，按前缀在后台手动删
    async fn remove_bucket(bucket_name: &str) {
        match AwsS3Store::from_config(&config(bucket_name, false), &Default::default()).await {
            Err(e) if e.is(S3Err::BucketNotFound) => return,
            Err(e) => {
                eprintln!("check bucket {bucket_name} before cleanup failed: {e:#}");
                return;
            }
            Ok(_) => {}
        }
        let removed = async {
            AwsS3Store::from_config(&config(bucket_name, true), &Default::default())
                .await?
                .default_client_default_bucket()
                .empty_and_delete_bucket()
                .await
        };
        if let Err(e) = removed.await {
            eprintln!("cleanup bucket {bucket_name} failed: {e:#}");
        }
    }

    #[tokio::test]
    async fn put_get_head_delete_object() {
        with_temp_bucket(|store, bucket_name| async move {
            let sdk = sdk_client(&config(&bucket_name, false).default);
            put_get_head_delete(&store, &sdk, &bucket_name).await;
        })
        .await;
    }

    /// 上传本地文件，再用字节流读回来
    #[tokio::test]
    async fn put_file_and_get() {
        with_temp_bucket(|store, _| async move {
            let bucket = store.default_client_default_bucket();
            let content: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
            let file = temp_file(&content, ".bin");
            let key = put_file(&bucket, PutFile::Path(file.path().to_owned()))
                .await
                .unwrap();

            let object = bucket.get(&key).await.unwrap().unwrap();
            assert_eq!(object.meta.size, Some(content.len() as i64));
            let mut reader = object.body.into_async_read();
            let mut written = Vec::new();
            tokio::io::copy(&mut reader, &mut written).await.unwrap();
            assert_eq!(written, content);
        })
        .await;
    }

    /// 本地文件算完摘要、上传之前被改了（长度不变）：带的 sha256 和服务端收到的内容对不上，拒绝写入
    #[tokio::test]
    async fn put_rejects_file_changed_after_digest() {
        with_temp_bucket(|store, _| async move {
            let bucket = store.default_client_default_bucket();
            let file = temp_file(b"hello", ".txt");
            let path = file.path().to_owned();
            let err = bucket
                .put(
                    KEY_PREFIX,
                    PutKey::Content,
                    PutFile::Path(path.clone()),
                    async |_| {
                        std::fs::write(&path, b"HELLO").unwrap();
                        Ok(PutDecision::<()>::Put)
                    },
                )
                .await
                .unwrap_err();
            assert!(err.is(S3Err::ServiceError), "{err:#}");
            let hello_key = format!("files/{HELLO_SHA256}.txt");
            assert!(bucket.get(&hello_key).await.unwrap().is_none());
        })
        .await;
    }

    /// 预签名 GET 读回对象（用 HTTP 实际访问）
    #[tokio::test]
    async fn presigned_get_reads_object() {
        with_temp_bucket(|store, _| async move {
            let bucket = store.default_client_default_bucket();
            let http = reqwest::Client::new();

            bucket
                .put_object(
                    "presigned.txt",
                    Bytes::from_static(b"via presigned"),
                    // `printf 'via presigned' | shasum -a 256 | cut -d' ' -f1 | xxd -r -p | base64`
                    "GZ+P8Jcw6uuDDQNpOwGSbjEihXOzU9hh4iTTpze+ChI=",
                    13,
                    "text/plain",
                )
                .await
                .unwrap();

            let get_url = bucket
                .presigned_get_url(
                    "presigned.txt",
                    Some(Duration::from_secs(600)),
                    PresignGetOptions::default(),
                )
                .await
                .unwrap();
            let response = http.get(&get_url).send().await.unwrap();
            assert!(response.status().is_success(), "{response:?}");
            assert_eq!(&response.bytes().await.unwrap()[..], b"via presigned");

            // 签进 URL 的 response-* 覆盖响应头
            let get_url = bucket
                .presigned_get_url("presigned.txt", Some(Duration::from_secs(600)), overrides())
                .await
                .unwrap();
            assert_overridden(&http.get(&get_url).send().await.unwrap());
        })
        .await;
    }

    /// 清空并删掉 bucket，之后 bucket 不存在
    #[tokio::test]
    async fn empty_and_delete_bucket() {
        with_temp_bucket(|store, bucket_name| async move {
            let sdk = sdk_client(&config(&bucket_name, false).default);
            let bucket = store.default_client_default_bucket();
            put_file(&bucket, bytes(Bytes::from_static(b"one"), Some("txt")))
                .await
                .unwrap();
            sdk.put_object()
                .bucket(&bucket_name)
                .key("outside/2.txt")
                .body(Bytes::from_static(b"two").into())
                .send()
                .await
                .unwrap();

            bucket.empty_and_delete_bucket().await.unwrap();
            let err = sdk
                .head_bucket()
                .bucket(&bucket_name)
                .send()
                .await
                .unwrap_err();
            assert!(err.into_service_error().is_not_found());
        })
        .await;
    }
}
