//! 组件 trait，以及 Registry 内部存放组件用的类型

use std::any::TypeId;
use std::time::Duration;

use async_trait::async_trait;
use tokio::task::JoinHandle;

use super::{CancellationToken, Name, ReadyResources, ResourceId, ResourceSink, Resources};
use crate::HyErr;

/// 应用组件的公共部分：怎么构建、提供和依赖哪些资源、怎么关闭。
///
/// 怎么启动由两个子 trait 之一决定，用 [`Kind`](Self::Kind) 选：
/// - [`Immediate`]：实现 [`ImmediateComponent`]，第一阶段就启动完成（数据库、Redis 等连接池）
/// - [`Deferred`]：实现 [`DeferredComponent`]，第一阶段只做准备，等所有组件第一阶段完成、
///   [`Registry::before_activate`](super::Registry::before_activate) 的回调执行完后，第二阶段才开始工作（HTTP / gRPC 服务、MQ 消费者、定时任务等）
///
/// 一般不手写这个 impl，而是在子 trait 的 impl 块上标 [`#[component]`](super::component)，
/// 把这里的项和启动方法写在一起，宏拆成两个 impl 并填好 `Kind`：
///
/// ```ignore
/// #[component]
/// impl ImmediateComponent for PgComponent {
///     type Config = PgConfig;
///     fn build(name: Name, config: PgConfig) -> Self { .. }
///     fn provides(&self) -> Vec<ResourceId> { .. }
///     async fn startup(&mut self, state: &Resources, shutdown: CancellationToken) -> .. { .. }
/// }
/// ```
///
/// 同一个组件类型可以用不同的名字注册多次（比如主库、从库两个 PgPool 组件），
/// 组件把自己的资源用 `insert_named(name, ..)` 放进去，按名字区分。匿名组件的名字是 `""`。
///
/// 组件之间的依赖用 [`provides`](Self::provides) / [`depends_on`](Self::depends_on) 声明，
/// Registry 按依赖排启动顺序（被依赖的先启动），`add` 的先后不重要；没有依赖关系的组件按 `add` 的顺序。
#[async_trait]
pub trait Component: Sized + Send + 'static {
    /// 组件的配置类型
    type Config;

    /// 启动方式：[`Immediate`] 或 [`Deferred`]，要和实现的子 trait 对上（[`ImmediateComponent`] /
    /// [`DeferredComponent`]），对不上编译不过。用 `#[component]` 时由宏按 impl 的 trait 填
    type Kind: sealed::Kind<Self>;

    /// 用名字和配置构建组件。
    ///
    /// 启动时要用到 `name` 的话存进结构体（比如 `state.insert_named(self.name.clone(), pool)`）。
    fn build(name: Name, config: Self::Config) -> Self;

    /// 启动后会放进 Resources 的资源，默认没有。Registry 据此知道谁提供什么，排启动顺序；
    /// 第一阶段（[`ImmediateComponent::startup`] / [`DeferredComponent::prepare`]）完会检查这些资源真的放进去了，
    /// 没放就算启动失败。
    ///
    /// ```ignore
    /// fn provides(&self) -> Vec<ResourceId> {
    ///     vec![ResourceId::named::<PgPool>(self.name.clone())]
    /// }
    /// ```
    fn provides(&self) -> Vec<ResourceId> {
        Vec::new()
    }

    /// 需要的资源，默认没有。没有任何组件提供、依赖成环、同一个资源有两个组件提供，
    /// 都会在启动任何组件之前报错。
    ///
    /// - [`Immediate`] 组件：Registry 把提供它们的组件排在前面启动，`startup` 里用
    ///   [`Resources::require_named`] 取出来
    /// - [`Deferred`] 组件：只在第二阶段读，那时所有组件的第一阶段都已经完成，
    ///   所以只检查有没有组件提供，不参与第一阶段的排序
    ///
    /// ```ignore
    /// fn depends_on(&self) -> Vec<ResourceId> {
    ///     vec![ResourceId::named::<PgPool>("primary"), ResourceId::of::<RedisClient>()]
    /// }
    /// ```
    fn depends_on(&self) -> Vec<ResourceId> {
        Vec::new()
    }

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

/// 启动方式：第一阶段就启动完成，组件实现 [`ImmediateComponent`]
pub struct Immediate;

/// 启动方式：等所有组件第一阶段完成、`before_activate` 的回调执行完后才开始工作，组件实现 [`DeferredComponent`]
pub struct Deferred;

/// [`Immediate`] 组件的启动
#[async_trait]
pub trait ImmediateComponent: Component {
    /// 启动组件。
    ///
    /// 可能失败的初始化都放在这里（连数据库等）。从 `state` 读依赖，
    /// 把自己的资源放进去给后面的组件用。需要后台任务就 spawn 并返回它的 handle，否则返回 `None`。
    /// 返回错误时，前面已经启动的组件会自动关闭。错误用 [`BaseAppErr`](super::BaseAppErr) 的变体，底层原因挂在 source 上。
    ///
    /// `shutdown` 是这个组件自己的退出信号，关闭时触发。后台任务里 `shutdown.cancelled().await` 等它，
    /// 收到后做完收尾就退出；在超时（[`Component::shutdown_timeout`]）内没退出的任务会被 abort
    async fn startup(
        &mut self,
        state: &Resources,
        shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>, HyErr>;
}

/// [`Deferred`] 组件的启动，分两个阶段
#[async_trait]
pub trait DeferredComponent: Component {
    /// 第一阶段：做准备，不开始工作。比如绑定端口、检查配置、连上 MQ 但不开始消费。
    ///
    /// 可能失败的初始化尽量放在这里，这样失败时别的 Deferred 组件都还没开始工作。
    /// `sink` 只能往 Resources 里放东西、读不了：这时别的组件可能还没启动完，要读的资源等 `activate`。
    /// 返回错误时，前面已经启动的组件会自动关闭
    async fn prepare(&mut self, sink: &ResourceSink) -> Result<(), HyErr>;

    /// 第二阶段：所有组件的第一阶段、`before_activate` 的回调都执行完了，开始工作（接请求、开始消费等）。
    ///
    /// `resources` 只读，可以克隆一份存进 handler 的 state。需要后台任务就 spawn 并返回它的 handle，
    /// 退出信号 `shutdown` 的用法同 [`ImmediateComponent::startup`]：
    ///
    /// ```ignore
    /// let handle = tokio::spawn(async move {
    ///     axum::serve(listener, router)
    ///         .with_graceful_shutdown(shutdown.cancelled_owned())
    ///         .await
    /// });
    /// ```
    async fn activate(
        &mut self,
        resources: ReadyResources,
        shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>, HyErr>;
}

/// 按 `Component::Kind` 分派到两个子 trait。模块私有，外部没法实现 `Kind`，
/// `Kind` 只能是 [`Immediate`] / [`Deferred`]
pub(super) mod sealed {
    use super::*;

    pub trait Kind<C> {
        /// 是不是 Deferred：排序和关闭顺序要区分
        const DEFERRED: bool;

        fn wrap(component: C) -> Box<dyn ComponentObject>;
    }

    impl<C: ImmediateComponent> Kind<C> for Immediate {
        const DEFERRED: bool = false;

        fn wrap(component: C) -> Box<dyn ComponentObject> {
            Box::new(ImmediateObject(component))
        }
    }

    impl<C: DeferredComponent> Kind<C> for Deferred {
        const DEFERRED: bool = true;

        fn wrap(component: C) -> Box<dyn ComponentObject> {
            Box::new(DeferredObject(component))
        }
    }

    /// 让不同类型的组件能放进同一个 Vec 的对象安全 trait，`Config`、`Kind` 到这里都已经擦掉了。
    /// `provides` 这些在 `add` 时就算好存进 [`ComponentEntry`]，这里只留生命周期方法
    #[async_trait]
    pub trait ComponentObject: Send + 'static {
        /// 第一阶段：Immediate 调 `startup`，Deferred 调 `prepare`
        async fn startup(
            &mut self,
            state: &Resources,
            shutdown: CancellationToken,
        ) -> Result<Option<JoinHandle<()>>, HyErr>;

        /// 第二阶段：Deferred 调 `activate`，Immediate 什么都不做
        async fn activate(
            &mut self,
            state: &Resources,
            shutdown: CancellationToken,
        ) -> Result<Option<JoinHandle<()>>, HyErr>;

        async fn stop(&mut self) -> Result<(), HyErr>;
    }

    struct ImmediateObject<C>(C);

    #[async_trait]
    impl<C: ImmediateComponent> ComponentObject for ImmediateObject<C> {
        async fn startup(
            &mut self,
            state: &Resources,
            shutdown: CancellationToken,
        ) -> Result<Option<JoinHandle<()>>, HyErr> {
            ImmediateComponent::startup(&mut self.0, state, shutdown).await
        }

        async fn activate(
            &mut self,
            _state: &Resources,
            _shutdown: CancellationToken,
        ) -> Result<Option<JoinHandle<()>>, HyErr> {
            Ok(None)
        }

        async fn stop(&mut self) -> Result<(), HyErr> {
            Component::stop(&mut self.0).await
        }
    }

    struct DeferredObject<C>(C);

    #[async_trait]
    impl<C: DeferredComponent> ComponentObject for DeferredObject<C> {
        async fn startup(
            &mut self,
            state: &Resources,
            _shutdown: CancellationToken,
        ) -> Result<Option<JoinHandle<()>>, HyErr> {
            DeferredComponent::prepare(&mut self.0, &ResourceSink::new(state)).await?;
            Ok(None)
        }

        async fn activate(
            &mut self,
            state: &Resources,
            shutdown: CancellationToken,
        ) -> Result<Option<JoinHandle<()>>, HyErr> {
            DeferredComponent::activate(&mut self.0, ReadyResources::new(state), shutdown).await
        }

        async fn stop(&mut self) -> Result<(), HyErr> {
            Component::stop(&mut self.0).await
        }
    }
}

/// Registry 里的一个组件。`type_id` + `name` 用来发现重复添加
pub(super) struct ComponentEntry {
    pub(super) type_id: TypeId,
    pub(super) name: Name,
    pub(super) type_name: &'static str,
    /// 是不是 Deferred 组件
    pub(super) deferred: bool,
    /// `add` 时从组件取出来存下：build 之后就不会变，排序、校验时不用再经过 dyn
    pub(super) provides: Vec<ResourceId>,
    pub(super) depends_on: Vec<ResourceId>,
    pub(super) shutdown_timeout: Option<Duration>,
    pub(super) component: Box<dyn sealed::ComponentObject>,
    /// 这个组件自己的退出信号，关闭时单独触发
    pub(super) shutdown: CancellationToken,
    /// 后台任务：Immediate 组件来自 `startup`，Deferred 组件来自 `activate`
    pub(super) handle: Option<JoinHandle<()>>,
}

impl ComponentEntry {
    pub(super) fn new<C: Component>(name: Name, config: C::Config) -> Self {
        let component = C::build(name.clone(), config);
        Self {
            type_id: TypeId::of::<C>(),
            name,
            type_name: std::any::type_name::<C>(),
            deferred: <C::Kind as sealed::Kind<C>>::DEFERRED,
            provides: component.provides(),
            depends_on: component.depends_on(),
            shutdown_timeout: component.shutdown_timeout(),
            component: <C::Kind as sealed::Kind<C>>::wrap(component),
            shutdown: CancellationToken::new(),
            handle: None,
        }
    }
}
