//! 组件 trait，以及 Registry 内部存放组件用的类型

use std::any::TypeId;
use std::time::Duration;

use async_trait::async_trait;
use tokio::task::JoinHandle;

use super::{CancellationToken, Name, ResourceId, Resources};
use crate::HyErr;

/// 应用组件：实现这一个 trait，同时定义组件怎么构建、怎么启动。
///
/// 同一个组件类型可以用不同的名字注册多次（比如主库、从库两个 PgPool 组件），
/// 组件把自己的资源用 `insert_named(name, ..)` 放进去，按名字区分。匿名组件的名字是 `""`。
///
/// 组件之间的依赖用 [`provides`](Self::provides) / [`depends_on`](Self::depends_on) 声明，
/// Registry 按依赖排启动顺序（被依赖的先启动），`add` 的先后不重要；没有依赖关系的组件按 `add` 的顺序。
#[async_trait]
pub trait Component: Send + 'static {
    /// 组件的配置类型
    type Config;

    /// 用名字和配置构建组件。
    ///
    /// `startup` 里要用到 `name` 的话存进结构体（比如 `state.insert_named(self.name.clone(), pool)`）。
    fn build(name: Name, config: Self::Config) -> Self;

    /// 启动后会放进 Resources 的资源，默认没有。Registry 据此知道谁提供什么，排启动顺序；
    /// 启动完会检查这些资源真的放进去了，没放就算启动失败。
    ///
    /// ```ignore
    /// fn provides(&self) -> Vec<ResourceId> {
    ///     vec![ResourceId::named::<PgPool>(self.name.clone())]
    /// }
    /// ```
    fn provides(&self) -> Vec<ResourceId> {
        Vec::new()
    }

    /// 启动前必须已经有的资源，默认没有。Registry 把提供它们的组件排在前面，关闭时排在后面；
    /// 没有任何组件提供、依赖成环、同一个资源有两个组件提供，都会在启动任何组件之前报错。
    /// `startup` 里用 [`Resources::require_named`] 取出来。
    ///
    /// ```ignore
    /// fn depends_on(&self) -> Vec<ResourceId> {
    ///     vec![ResourceId::named::<PgPool>("primary"), ResourceId::of::<RedisClient>()]
    /// }
    /// ```
    fn depends_on(&self) -> Vec<ResourceId> {
        Vec::new()
    }

    /// 启动组件。
    ///
    /// 可能失败的初始化都放在这里（绑定端口、连数据库等）。从 `state` 读依赖，
    /// 把自己的资源放进去给后面的组件用。需要后台任务就 spawn 并返回它的 handle，否则返回 `None`。
    /// 返回错误时，前面已经启动的组件会自动关闭。错误用 [`BaseAppErr`](super::BaseAppErr) 的变体，底层原因挂在 source 上。
    ///
    /// `shutdown` 是这个组件自己的退出信号，关闭时按启动的逆序逐个触发（先停后启动的，
    /// 也就是先停依赖别人的）。后台任务里 `shutdown.cancelled().await` 等它，收到后做完收尾就退出；
    /// 在超时（[`Component::shutdown_timeout`]，默认 [`RegistryConfig::shutdown_timeout_secs`](super::RegistryConfig::shutdown_timeout_secs)）内
    /// 没退出的任务会被 abort：
    ///
    /// ```ignore
    /// let handle = tokio::spawn(async move {
    ///     axum::serve(listener, router)
    ///         .with_graceful_shutdown(shutdown.cancelled_owned())
    ///         .await
    /// });
    /// ```
    async fn startup(
        &mut self,
        state: &Resources,
        shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>, HyErr>;

    /// 关闭时的收尾，在后台任务结束之后调用，默认什么都不做。
    ///
    /// 给没有后台任务、但持有外部资源的组件用，比如连接池：在这里关掉连接（等在途查询完成），
    /// 数据库那边看到的是正常断开，而不是进程退出导致的连接异常中断。返回错误只打 WARN，
    /// 不影响关后面的组件
    async fn stop(&mut self) -> Result<(), HyErr> {
        Ok(())
    }

    /// 关闭这个组件最多等多久（等后台任务结束 + `stop`），`None` 用 [`RegistryConfig::shutdown_timeout_secs`](super::RegistryConfig::shutdown_timeout_secs)。
    /// 有长请求要处理完的组件（HTTP、gRPC）可以给长一点，一般从组件自己的配置里读
    fn shutdown_timeout(&self) -> Option<Duration> {
        None
    }
}

/// Registry 里的一个组件。`type_id` + `name` 用来发现重复添加
pub(super) struct ComponentEntry {
    pub(super) type_id: TypeId,
    pub(super) name: Name,
    pub(super) type_name: &'static str,
    pub(super) component: Box<dyn ComponentObject>,
    /// 这个组件自己的退出信号，关闭时单独触发
    pub(super) shutdown: CancellationToken,
    /// 启动后返回的后台任务
    pub(super) handle: Option<JoinHandle<()>>,
}

/// 让不同类型的组件能放进同一个 Vec 的对象安全 trait。
/// 只暴露 `startup`；`build` 和 `Config` 不是对象安全的。
#[async_trait]
pub(super) trait ComponentObject: Send + 'static {
    async fn startup(
        &mut self,
        state: &Resources,
        shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>, HyErr>;

    async fn stop(&mut self) -> Result<(), HyErr>;

    fn shutdown_timeout(&self) -> Option<Duration>;

    fn provides(&self) -> Vec<ResourceId>;

    fn depends_on(&self) -> Vec<ResourceId>;
}

#[async_trait]
impl<C: Component> ComponentObject for C {
    async fn startup(
        &mut self,
        state: &Resources,
        shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>, HyErr> {
        Component::startup(self, state, shutdown).await
    }

    async fn stop(&mut self) -> Result<(), HyErr> {
        Component::stop(self).await
    }

    fn shutdown_timeout(&self) -> Option<Duration> {
        Component::shutdown_timeout(self)
    }

    fn provides(&self) -> Vec<ResourceId> {
        Component::provides(self)
    }

    fn depends_on(&self) -> Vec<ResourceId> {
        Component::depends_on(self)
    }
}
