//! S3 客户端连真实服务读写对象，分两层（见仓库根目录 AGENTS.md 的测试分层）：
//!
//! - `#[container]`：用代码启动 RustFS 容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）
//!   ```sh
//!   cargo test -p hygiea-aws --features s3 --test s3 -- --ignored _container::
//!   ```
//! - `#[live]`：连自己的 S3 兼容服务（AWS S3、Cloudflare R2 等），需要一个可写的 bucket
//!   ```sh
//!   export S3_ENDPOINT_URL=https://<ACCOUNT_ID>.r2.cloudflarestorage.com  # 连 AWS 时不设
//!   export S3_REGION=auto
//!   export S3_BUCKET=my-bucket
//!   export S3_ACCESS_KEY_ID=...
//!   export S3_SECRET_ACCESS_KEY=...
//!   export S3_FORCE_PATH_STYLE=false  # MinIO / RustFS 设 true
//!
//!   cargo test -p hygiea-aws --features s3 --test s3 -- --ignored _live::
//!   ```

use hygiea_aws::aws_sdk_s3::primitives::ByteStream;
use hygiea_aws::{AwsConfig, AwsS3Client, S3Config, StaticCredentials};
use hygiea_core::app::ConfigResource;
use hygiea_test::{container, live};

/// 上传、读回、删除一个对象，删完确认对象不在了
async fn put_get_delete(client: &AwsS3Client, bucket: &str) {
    let key = "hygiea-test/put_get_delete.txt";

    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(ByteStream::from_static(b"hello hygiea"))
        .send()
        .await
        .unwrap();

    let body = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .unwrap()
        .body
        .collect()
        .await
        .unwrap()
        .into_bytes();
    assert_eq!(&body[..], b"hello hygiea");

    client
        .delete_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .unwrap();

    let err = client
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .unwrap_err();
    assert!(err.into_service_error().is_not_found());
}

/// 用代码启动的 RustFS（兼容 S3 的对象存储）
#[container]
mod rustfs_container {
    use hygiea_test::container::{ContainerSpec, RunningContainer};

    use super::*;

    const ACCESS_KEY: &str = "hygiea-test";
    const SECRET_KEY: &str = "hygiea-test-secret";
    const PORT: u16 = 9000;

    /// 镜像固定版本，`/health` 返回 200 算就绪
    async fn start() -> RunningContainer {
        ContainerSpec::new("rustfs/rustfs", "1.0.0")
            .env("RUSTFS_ACCESS_KEY", ACCESS_KEY)
            .env("RUSTFS_SECRET_KEY", SECRET_KEY)
            .port(PORT)
            .wait_http(PORT, "/health", 200)
            .start()
            .await
    }

    /// 自建服务的写法：path-style，region 随便填
    fn config(server: &RunningContainer, secret_key: &str) -> AwsConfig {
        AwsConfig {
            region: Some("us-east-1".into()),
            endpoint_url: Some(format!("http://{}", server.addr(PORT))),
            credentials: Some(StaticCredentials {
                access_key_id: ACCESS_KEY.into(),
                secret_access_key: secret_key.into(),
                session_token: None,
            }),
            s3: S3Config {
                force_path_style: true,
                ..S3Config::default()
            },
            ..AwsConfig::default()
        }
    }

    #[tokio::test]
    async fn put_get_delete_object() {
        let server = start().await;
        let client = AwsS3Client::from_config(&config(&server, SECRET_KEY))
            .await
            .unwrap();
        client.create_bucket().bucket("test").send().await.unwrap();
        put_get_delete(&client, "test").await;
    }

    /// 密钥不对时服务端拒绝签名：确认请求真的到了服务端并做了校验
    #[tokio::test]
    async fn wrong_secret_is_rejected() {
        let server = start().await;
        let client = AwsS3Client::from_config(&config(&server, "wrong-secret"))
            .await
            .unwrap();
        let err = client.list_buckets().send().await.unwrap_err();
        assert_eq!(
            err.as_service_error().and_then(|e| e.meta().code()),
            Some("SignatureDoesNotMatch"),
            "{err:?}"
        );
    }
}

/// 自己的 S3 兼容服务，连接信息全部从环境变量读
#[live(env = ["S3_REGION", "S3_BUCKET", "S3_ACCESS_KEY_ID", "S3_SECRET_ACCESS_KEY"])]
mod own_bucket_live {
    use super::*;

    /// 环境变量在 `#[live(env = ..)]` 里检查过了，这里取不到只可能是宏的检查漏了
    fn env(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| panic!("missing env {key}"))
    }

    fn config() -> AwsConfig {
        AwsConfig {
            region: Some(env("S3_REGION")),
            endpoint_url: std::env::var("S3_ENDPOINT_URL").ok(),
            credentials: Some(StaticCredentials {
                access_key_id: env("S3_ACCESS_KEY_ID"),
                secret_access_key: env("S3_SECRET_ACCESS_KEY"),
                session_token: None,
            }),
            s3: S3Config {
                force_path_style: std::env::var("S3_FORCE_PATH_STYLE").is_ok_and(|v| v == "true"),
                ..S3Config::default()
            },
            ..AwsConfig::default()
        }
    }

    #[tokio::test]
    async fn put_get_delete_object() {
        let client = AwsS3Client::from_config(&config()).await.unwrap();
        put_get_delete(&client, &env("S3_BUCKET")).await;
    }
}
