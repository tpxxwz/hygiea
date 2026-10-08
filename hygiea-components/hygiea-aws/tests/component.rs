//! `AwsS3Component` 经 Registry 启动：按组件名提供 `AwsS3Client`，不连网络
//!
//! 运行：`cargo test -p hygiea-aws --features s3 --test component`

use hygiea_aws::{AwsConfig, AwsS3Client, AwsS3Component};
use hygiea_core::app::{Component, Name, Registry, ResourceId, Resources};

fn config(region: &str) -> AwsConfig {
    AwsConfig {
        region: Some(region.into()),
        ..AwsConfig::default()
    }
}

/// 按配置构造：按组件名声明 S3 客户端
#[test]
fn build_declares_named_resource() {
    let component = AwsS3Component::build(Name::from("r2"), config("auto"));
    assert_eq!(
        component.provides(),
        vec![ResourceId::named::<AwsS3Client>("r2")]
    );
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

/// 匿名添加：`get::<..>()` 就能取到
#[tokio::test]
async fn anonymous_component_provides_client() {
    let registry = Registry::new().add::<AwsS3Component>(config("us-east-1"));
    let found = with_resources(registry, |res| res.get::<AwsS3Client>().is_some()).await;
    assert!(found);
}

/// 多个实例各用各的配置，按组件名区分
#[tokio::test]
async fn each_named_instance_uses_its_own_config() {
    let registry = Registry::new()
        .add_named::<AwsS3Component>("aws", config("us-east-1"))
        .add_named::<AwsS3Component>("r2", config("auto"));
    let (aws, r2, anonymous) = with_resources(registry, |res| {
        let region = |name: &str| {
            res.get_named::<AwsS3Client>(name)
                .and_then(|c| c.config().region().map(|r| r.to_string()))
        };
        (
            region("aws"),
            region("r2"),
            res.get::<AwsS3Client>().is_some(),
        )
    })
    .await;
    assert_eq!(aws.as_deref(), Some("us-east-1"));
    assert_eq!(r2.as_deref(), Some("auto"));
    assert!(!anonymous);
}
