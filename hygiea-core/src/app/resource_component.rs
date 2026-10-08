//! 一个配置建一个资源的组件：资源实现 [`ConfigResource`]，组件直接用 [`ImmediateResourceComponent`]
//!
//! 数据库、Redis 连接池、HTTP 客户端这类组件做的事都一样：启动时按配置建出资源、按组件名放进 Resources，
//! 关闭时把资源关掉。差别只在资源怎么建、怎么关，所以组件只写这两步，其余的由
//! [`ImmediateResourceComponent`] 统一做：
//!
//! ```ignore
//! pub struct SqlxPgPool {
//!     inner: PgPool,
//! }
//!
//! #[async_trait]
//! impl ConfigResource for SqlxPgPool {
//!     type Config = SqlxPgConfig;
//!
//!     async fn from_config(config: &SqlxPgConfig) -> Result<Self> { .. }
//!
//!     async fn close(&self) -> Result<()> { .. }
//! }
//!
//! pub type SqlxPgComponent = ImmediateResourceComponent<SqlxPgPool>;
//! ```
//!
//! 不经过 Registry 时直接调 `SqlxPgPool::from_config(&config)`。一个配置建出多个资源、要跑后台任务、
//! Deferred 组件这些情况照常手写组件。

use async_trait::async_trait;
use tokio::task::JoinHandle;

use super::{CancellationToken, ImmediateComponent, Name, Resource, ResourceId, Resources, component};
use crate::Result;

/// 能从一份配置直接建出来的资源。
///
/// [`from_config`](Self::from_config) 不依赖 Registry，测试、脚本里可以直接调。
/// 资源里不存完整的配置：运行时要用的值在 `from_config` 里转换好，作为单独的字段存放
#[async_trait]
pub trait ConfigResource: Resource + Sized {
    /// 建资源用的配置，也是 [`ImmediateResourceComponent`] 的配置
    type Config: Send + Sync + 'static;

    /// 按配置建出资源（连数据库、建客户端等）。错误用 [`AppErr`](super::AppErr) 的变体，底层原因挂在 source 上
    async fn from_config(config: &Self::Config) -> Result<Self>;

    /// 关闭资源，组件关闭时调用，比如等在途查询跑完后关掉连接池。默认什么都不做
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

/// 一个配置建一个资源的 Immediate 组件：`startup` 调 [`ConfigResource::from_config`]，
/// 资源按组件名放进 Resources；`stop` 调 [`ConfigResource::close`]。
///
/// 同一个组件可以用不同的名字注册多次（比如主库、从库），资源按组件名放进 Resources，
/// 用 `get_named::<R>(name)` 取；匿名注册（名字是 `""`）的用 `get::<R>()` 取。
/// 依赖它的组件声明 `ResourceId::named::<R>(名字)`
pub struct ImmediateResourceComponent<R: ConfigResource> {
    name: Name,
    config: R::Config,
    /// 启动后留一份，关闭时在 `stop` 里关掉
    resource: Option<R>,
}

#[component]
impl<R: ConfigResource> ImmediateComponent for ImmediateResourceComponent<R> {
    type Config = R::Config;

    fn build(name: Name, config: Self::Config) -> Self {
        Self {
            name,
            config,
            resource: None,
        }
    }

    fn provides(&self) -> Vec<ResourceId> {
        vec![ResourceId::named::<R>(self.name.clone())]
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        let resource = R::from_config(&self.config).await?;
        resources.insert_named(self.name.clone(), resource.clone());
        self.resource = Some(resource);
        Ok(None)
    }

    async fn stop(&mut self) -> Result<()> {
        if let Some(resource) = self.resource.take() {
            resource.close().await?;
        }
        Ok(())
    }
}
