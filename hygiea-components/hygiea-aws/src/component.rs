//! 应用组件 [`AwsComponent`]：加载公共配置，造出开了 feature 的各服务客户端，按组件名放进 Resources。

use aws_config::SdkConfig;
use hygiea_core::HyErr;
use hygiea_core::app::{
    CancellationToken, ImmediateComponent, Name, ResourceId, Resources, component,
};
use tokio::task::JoinHandle;

use crate::AwsConfig;

/// 同一个组件可以用不同名字注册多次，`SdkConfig` 和各服务客户端都按组件名放进 Resources，
/// 用 `get_named::<..>(name)` 取；匿名注册的用 `get::<..>()` 取
pub struct AwsComponent {
    name: Name,
    config: AwsConfig,
}

#[component]
impl ImmediateComponent for AwsComponent {
    type Config = AwsConfig;

    fn build(name: Name, config: Self::Config) -> Self {
        Self { name, config }
    }

    fn provides(&self) -> Vec<ResourceId> {
        vec![
            ResourceId::named::<SdkConfig>(self.name.clone()),
            #[cfg(feature = "s3")]
            ResourceId::named::<aws_sdk_s3::Client>(self.name.clone()),
        ]
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>, HyErr> {
        let sdk_config = self.config.load().await;
        tracing::info!(
            "AWS config loaded [region={:?}]",
            sdk_config.region().map(|r| r.as_ref())
        );
        #[cfg(feature = "s3")]
        resources.insert_named(self.name.clone(), self.config.s3.client(&sdk_config));
        resources.insert_named(self.name.clone(), sdk_config);
        Ok(None)
    }
}
