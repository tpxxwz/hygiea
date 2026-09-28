//! Registry：组件的注册、启动、运行、逆序关闭

use std::any::TypeId;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use super::component::ComponentEntry;
use super::signal::{ShutdownSignals, force_exit};
use super::{BaseAppErr, Component, Name, RegistryConfig, ResourceId, Resources};
use crate::log::{BaseLogErr, LogGuard};
use crate::{HyErr, err};

/// [`Registry::before_activate`] / [`Registry::on_ready`] 的回调，装箱后存进 Registry
type Hook =
    Box<dyn FnOnce(Resources) -> Pin<Box<dyn Future<Output = Result<(), HyErr>> + Send>> + Send>;

/// 组件注册表：按注册顺序启动组件，收到退出信号后关闭。
///
/// 用法是 `add` 组件、按需设置 `before_activate` / `on_ready`，最后 `run().await`：
/// 不调用 `run` 什么都不会发生，所以标了 `#[must_use]`，漏了编译器会警告
#[must_use = "Registry 在调用 `run().await` 之前什么都不做"]
pub struct Registry {
    state: Resources,
    components: Vec<ComponentEntry>,
    /// 第一阶段完成后、Deferred 组件 activate 之前调用，见 [`Registry::before_activate`]
    before_activate: Option<Hook>,
    /// 所有组件都启动完、开始对外服务之后调用，见 [`Registry::on_ready`]
    on_ready: Option<Hook>,
    /// 启动成功的组件数，也就是 `components` 的前多少个；关闭时只关这些
    started: usize,
    /// 组件没单独指定时，关闭每个组件最多等多久
    shutdown_timeout: Duration,
    shutdown_delay: Duration,
    /// 日志的 guard：它活着时后台线程负责把日志写进文件，drop 时把缓冲里剩下的刷出去。
    /// `run` 结束时交给调用方，让 `run` 之后的日志也能写进文件。放在最后一个字段，
    /// 字段按声明顺序 drop，没走到 `run` 就被丢弃时，组件在 `Drop` 里打的日志也能写进去。
    /// 同一进程里日志已经被前一个 Registry 装过时是 `None`
    log_guard: Option<LogGuard>,
}

impl Registry {
    /// 用默认配置创建（日志只输出到控制台，级别 `info`）。
    // 不实现 Default：创建时会初始化全局日志，失败还会 panic，不适合藏在 Default 里
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::with_config(RegistryConfig::default())
    }

    /// 用指定的框架级配置创建。
    ///
    /// 拿到配置后立即初始化日志，之后组件启动时的日志都能正常输出。
    ///
    /// 日志装的是进程级的全局 subscriber，一个进程只能装一次：
    /// - 已经装过（比如同一个测试二进制里前面的测试已经建过 Registry，`cargo test` 默认在一个进程里
    ///   多线程并行跑）：静默沿用已经装好的，这次的日志配置不生效，Registry 照常创建；
    /// - 其他失败（文件 layer 的文件名重叠、日志目录建不了等配置问题）：日志还没装好，只能用 stderr 报错，
    ///   然后 panic（启动期错误，和 `load_config` 读配置失败一样）
    pub fn with_config(config: RegistryConfig) -> Self {
        let log_guard = match crate::log::init(&config.tracing) {
            Ok(guard) => Some(guard),
            // 同一进程里已经装过（多个测试各建一个 Registry），静默沿用已装好的
            Err(e) if e.is(BaseLogErr::InstallFailed) => None,
            Err(e) => {
                eprintln!("ERROR hygiea: {e:#}");
                panic!("{e:#}")
            }
        };
        Self {
            state: Resources::new(),
            components: Vec::new(),
            before_activate: None,
            on_ready: None,
            started: 0,
            shutdown_timeout: Duration::from_secs(config.shutdown_timeout_secs),
            shutdown_delay: Duration::from_secs(config.shutdown_delay_secs),
            log_guard,
        }
    }

    /// 添加匿名组件（名字是 `""`）
    // 构建器风格的方法，和 `std::ops::Add` 无关，clippy 这条是误报
    #[allow(clippy::should_implement_trait)]
    pub fn add<C: Component>(self, config: C::Config) -> Self {
        self.add_named::<C>("", config)
    }

    /// 添加具名组件，同一类型可以用不同的名字添加多个。
    ///
    /// `name` 会传给 `Component::build`，组件可以存下来在 `startup` 里用
    /// （比如 `state.insert_named(self.name.clone(), pool)`）。
    /// 同一类型、同一名字重复添加会 panic：两个实例会往 Resources 里放同名的资源
    pub fn add_named<C: Component>(mut self, name: impl Into<Name>, config: C::Config) -> Self {
        let name = name.into();
        let type_id = TypeId::of::<C>();
        let type_name = std::any::type_name::<C>();
        if self
            .components
            .iter()
            .any(|entry| entry.type_id == type_id && entry.name == name)
        {
            panic!("component {type_name}({name}) already added");
        }
        self.components.push(ComponentEntry::new::<C>(name, config));
        self
    }

    /// 设置所有组件第一阶段完成后、Deferred 组件 activate 之前调用的回调。
    ///
    /// 这时资源都齐了（包括 Deferred 组件在 `prepare` 里放的），还没有组件在对外服务，
    /// 适合建表、预热缓存、把资源收进全局状态：HTTP / gRPC 开始接请求时，这些一定已经做完。
    /// 往 `Resources` 里放的资源，Deferred 组件在 `activate` 里能读到；这类资源不属于任何组件，
    /// 不能写进 `depends_on`，在 `activate` 里直接取。
    ///
    /// 返回 `Err` 时和组件启动失败一样：关闭已启动的组件，`run` 返回这个错误。不设置就跳过这一步。
    /// 只能设置一次，重复设置会 panic（和重复 `add_named` 一样，基本都是写错了）
    ///
    /// ```ignore
    /// registry
    ///     .add::<SqlxPgComponent>(cfg.db)
    ///     .add::<AxumComponent>(cfg.http)
    ///     .before_activate(|res| async move { AppState::setup(&res).await })
    ///     .run()
    ///     .await
    /// ```
    pub fn before_activate<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce(Resources) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), HyErr>> + Send + 'static,
    {
        if self.before_activate.is_some() {
            panic!("before_activate already set");
        }
        self.before_activate = Some(Box::new(move |resources| Box::pin(f(resources))));
        self
    }

    /// 设置所有组件都启动完之后调用的回调：Deferred 组件已经 activate，服务已经在接请求。
    ///
    /// 适合注册到注册中心、打"已就绪"的点、请求一下自己做自检。要在对外服务之前做完的初始化
    /// （建表、初始化全局状态）放进 [`before_activate`](Self::before_activate)，放在这里时请求可能比它先到。
    ///
    /// 返回 `Err` 时和组件启动失败一样：关闭所有组件，`run` 返回这个错误。不设置就跳过这一步。
    /// 只能设置一次，重复设置会 panic
    pub fn on_ready<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce(Resources) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), HyErr>> + Send + 'static,
    {
        if self.on_ready.is_some() {
            panic!("on_ready already set");
        }
        self.on_ready = Some(Box::new(move |resources| Box::pin(f(resources))));
        self
    }

    /// 分两个阶段启动所有组件，然后一直运行到收到退出信号。
    ///
    /// 1. 第一阶段：按依赖顺序调 [`ImmediateComponent::startup`](super::ImmediateComponent::startup) / [`DeferredComponent::prepare`](super::DeferredComponent::prepare)，
    ///    每个组件完了检查它声明的 `provides` 都放进去了；
    /// 2. 调 [`before_activate`](Self::before_activate) 设置的回调（没设置就跳过）：资源都齐了，
    ///    Deferred 组件（HTTP / gRPC 等）还没开始工作；
    /// 3. 第二阶段：按同样的顺序调 Deferred 组件的 [`DeferredComponent::activate`](super::DeferredComponent::activate)，开始对外服务；
    /// 4. 调 [`on_ready`](Self::on_ready) 设置的回调（没设置就跳过）：服务已经在接请求。
    ///
    /// 两个回调拿到的 `Resources` 都是共享的同一份，克隆很便宜；返回 `Err` 时和组件启动失败一样，
    /// 关闭已启动的组件后返回。
    ///
    /// 退出信号：Linux / macOS 是 Ctrl+C（SIGINT）和 SIGTERM（`docker stop`、k8s 发的都是它）；
    /// Windows 是 Ctrl+C、Ctrl+Break、关闭控制台窗口和系统关机。信号在 `run` 一开始就开始监听，
    /// 组件启动期间收到的也不会丢：等启动完直接进入关闭。收到后：
    /// 1. 先照常服务 [`RegistryConfig::shutdown_delay_secs`]（默认 0，k8s 下一般配 5），
    ///    让负载均衡器把流量切走；
    /// 2. 逐个关组件：触发它的 `shutdown` 信号，等它的后台任务结束，调它的
    ///    [`Component::stop`]（比如关连接池），再关下一个。先按启动的逆序关 Deferred 组件
    ///    （HTTP / gRPC 不再接新请求，处理完手上的），再按启动的逆序关 Immediate 组件
    ///    （被依赖的后关，数据库最后提交完数据）；
    /// 3. 每个组件单独计时，最多等 [`Component::shutdown_timeout`]（默认
    ///    [`RegistryConfig::shutdown_timeout_secs`] = 15 秒），超时的 abort 并打 WARN，接着关下一个；
    /// 4. 上面任何阶段再收到一次信号，直接退出进程（退出码 130）。k8s 里只会发一次 SIGTERM，
    ///    这条主要给本地开发：关闭卡住时再按一次 Ctrl+C 就能退出。
    ///
    /// 运行中如果某个组件的后台任务自己结束了（panic 或提前退出），同样关闭所有组件，
    /// 返回 [`BaseAppErr::TaskExited`]，让进程非 0 退出、由 k8s / systemd 重启。
    ///
    /// 启动完成后会打出最长关闭时间（delay + 各组件超时之和）。部署到 k8s 时，
    /// `terminationGracePeriodSeconds` 要比它大，否则关到一半会被 SIGKILL。
    ///
    /// 返回 `(结果, 日志 guard)`：
    /// - 结果：组件第一阶段失败时是 [`BaseAppErr::ComponentStartFailed`]，第二阶段失败时是
    ///   [`BaseAppErr::ComponentActivateFailed`]，`before_activate` / `on_ready` 的回调失败时是它们返回的错误。
    ///   这时错误已经打过日志，调用方只需要决定怎么退出（比如 `main` 直接返回这个 `Err`，
    ///   退出码非 0），不用再打一遍；
    /// - 日志 guard：**要持有到 `main` 结束**。日志写文件靠它背后的线程，guard 一丢，
    ///   之后打的日志就写不进文件了（控制台照常输出）。同一进程里日志已经被别的 Registry
    ///   装过时是 `None`。
    ///
    /// ```ignore
    /// #[tokio::main]
    /// async fn main() -> Result<(), HyErr> {
    ///     let (registry, cfg) = Registry::load_config::<AppConfig>(&ConfigArgs::from_cli());
    ///     let (result, _log_guard) = registry.run().await;
    ///     tracing::info!("bye");   // _log_guard 还活着，能写进文件
    ///     result
    /// }
    /// ```
    ///
    /// 注意别写成 `let (result, _) = ..`：`_` 会让 guard 立刻被丢掉
    #[must_use = "持有返回的日志 guard 到 main 结束，并处理结果"]
    pub async fn run(mut self) -> (Result<(), HyErr>, Option<LogGuard>) {
        let result = self.run_until_shutdown().await;
        // guard 交出去之后 self 才 drop，组件在 Drop 里打的日志照样能写进文件
        (result, self.log_guard.take())
    }

    async fn run_until_shutdown(&mut self) -> Result<(), HyErr> {
        // 最先装好信号监听：之后组件启动期间收到的信号会留着，不会按系统默认行为直接杀掉进程
        let mut signals = ShutdownSignals::install();

        // 先按依赖关系排好启动顺序，缺依赖、成环、重复提供都在这里报，这时还没有组件启动，不用关
        if let Err(e) = self.sort_by_dependencies() {
            tracing::error!("{e:#}");
            return Err(e);
        }
        if let Err(e) = self.start_components().await {
            tracing::error!("{e:#}");
            self.shutdown_or_force_exit(&mut signals).await;
            return Err(e);
        }
        if let Some(before_activate) = self.before_activate.take()
            && let Err(e) = before_activate(self.state.clone()).await
        {
            tracing::error!("before_activate failed: {e:#}");
            self.shutdown_or_force_exit(&mut signals).await;
            return Err(e);
        }
        if let Err(e) = self.activate_components().await {
            tracing::error!("{e:#}");
            self.shutdown_or_force_exit(&mut signals).await;
            return Err(e);
        }
        if let Some(on_ready) = self.on_ready.take()
            && let Err(e) = on_ready(self.state.clone()).await
        {
            tracing::error!("on_ready failed: {e:#}");
            self.shutdown_or_force_exit(&mut signals).await;
            return Err(e);
        }
        self.log_max_shutdown_time();
        tracing::info!("Application ready");

        // 等退出信号的同时盯着各组件的后台任务：任务自己结束了说明应用已经不完整（比如 HTTP 服务挂了），
        // 这时关掉所有组件并返回错误，进程非 0 退出，交给 k8s / systemd 按策略重启，不留个"半死"的进程
        let signal = tokio::select! {
            signal = signals.recv() => signal,
            (idx, result) = self.any_task_exited() => {
                let err = self.task_exited_error(idx, result);
                tracing::error!("{err:#}, shutting down");
                self.shutdown_or_force_exit(&mut signals).await;
                return Err(err);
            }
        };
        tracing::info!("Received {signal}, starting graceful shutdown...");
        if !self.shutdown_delay.is_zero() {
            tracing::info!(
                "Still serving for {:?} (shutdown_delay) so load balancers can stop routing here",
                self.shutdown_delay
            );
            tokio::select! {
                () = tokio::time::sleep(self.shutdown_delay) => {}
                signal = signals.recv() => force_exit(signal),
            }
        }
        self.shutdown_or_force_exit(&mut signals).await;
        tracing::info!("Shutdown complete.");
        Ok(())
    }

    /// 按 `provides` / `depends_on` 把组件排成启动顺序（拓扑排序），直接重排 `components`。
    ///
    /// - Immediate 组件 A 依赖的资源由 B 提供，B 就排在 A 前面；没有依赖关系的保持 `add` 的先后
    /// - Deferred 组件的依赖只在第二阶段读，那时第一阶段都完成了，所以只检查有没有组件提供，不参与排序
    /// - 依赖的资源没有组件提供：[`BaseAppErr::ResourceMissing`]
    /// - 同一个资源两个组件都提供：[`BaseAppErr::DuplicateProvider`]
    /// - 依赖成环：[`BaseAppErr::DependencyCycle`]，报出环上的组件
    ///
    /// 关闭按启动的逆序，所以排好之后关闭顺序也自然是对的
    fn sort_by_dependencies(&mut self) -> Result<(), HyErr> {
        let n = self.components.len();
        let label = |idx: usize| {
            let entry = &self.components[idx];
            format!("{}({})", entry.type_name, entry.name)
        };

        // 谁提供哪个资源
        let mut provider: std::collections::HashMap<ResourceId, usize> =
            std::collections::HashMap::new();
        for idx in 0..n {
            for id in self.components[idx].provides.iter().cloned() {
                if let Some(&first) = provider.get(&id) {
                    return Err(err!(BaseAppErr::DuplicateProvider, {
                        "resource": id.to_string(),
                        "first": label(first),
                        "second": label(idx),
                    }));
                }
                provider.insert(id, idx);
            }
        }

        // after[b] 里是依赖 b 的组件：b 启动后它们才能启动
        let mut after: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut pending_deps = vec![0usize; n];
        for (idx, pending) in pending_deps.iter_mut().enumerate() {
            let mut deps: Vec<usize> = Vec::new();
            let deferred = self.components[idx].deferred;
            for id in &self.components[idx].depends_on {
                match provider.get(id) {
                    Some(_) if deferred => {}
                    Some(&dep) => deps.push(dep),
                    None => {
                        return Err(err!(
                            BaseAppErr::ResourceMissing,
                            format!("{id}, needed by {}; no component provides it", label(idx))
                        ));
                    }
                }
            }
            deps.sort_unstable();
            deps.dedup();
            *pending = deps.len();
            for dep in deps {
                after[dep].push(idx);
            }
        }

        // Kahn：每次从"依赖都已排好"的组件里挑 add 最早的，没有依赖关系的就保持 add 的先后
        let mut ready: std::collections::BTreeSet<usize> =
            (0..n).filter(|&idx| pending_deps[idx] == 0).collect();
        let mut order: Vec<usize> = Vec::with_capacity(n);
        while let Some(idx) = ready.pop_first() {
            order.push(idx);
            for &next in &after[idx] {
                pending_deps[next] -= 1;
                if pending_deps[next] == 0 {
                    ready.insert(next);
                }
            }
        }

        if order.len() < n {
            // 排不完说明剩下的组件里有环；从任意一个没排上的组件沿着依赖往回走，走回到见过的就是环
            let cycle = find_cycle(&after, &pending_deps)
                .into_iter()
                .map(label)
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(err!(BaseAppErr::DependencyCycle, cycle));
        }

        if order.iter().enumerate().any(|(pos, &idx)| pos != idx) {
            let names: Vec<String> = order.iter().map(|&idx| label(idx)).collect();
            tracing::info!("Start order by dependencies: {}", names.join(" -> "));
        }
        let mut slots: Vec<Option<ComponentEntry>> = std::mem::take(&mut self.components)
            .into_iter()
            .map(Some)
            .collect();
        self.components = order
            .into_iter()
            .filter_map(|idx| slots[idx].take())
            .collect();
        Ok(())
    }

    async fn start_components(&mut self) -> Result<(), HyErr> {
        for idx in 0..self.components.len() {
            let entry = &mut self.components[idx];
            let (name, type_name) = (entry.name.clone(), entry.type_name);
            let result = entry
                .component
                .startup(&self.state, entry.shutdown.clone())
                .await;

            match result {
                Ok(handle) => {
                    let has_task = handle.is_some();
                    entry.handle = handle;
                    // 先记为已启动：下面校验失败时，这个组件也要被关掉
                    self.started = idx + 1;
                    // 声明了 provides 就必须真的放进去，否则依赖它的组件会在后面取不到
                    let missing = entry.provides.iter().find(|id| !self.state.contains_id(id));
                    if let Some(id) = missing {
                        let cause = err!(
                            BaseAppErr::ResourceMissing,
                            format!("{id}, declared in provides() but not inserted by startup")
                        );
                        return Err(err!(BaseAppErr::ComponentStartFailed, {
                            "index": idx,
                            "type_name": type_name,
                            "name": name,
                        })
                        .with_source(cause));
                    }
                    if has_task {
                        tracing::info!("{type_name}({name}) started with background task");
                    } else {
                        tracing::info!("{type_name}({name}) started");
                    }
                }
                // 这里不打日志、不关组件，由 run 统一处理，避免同一个错误记两次
                Err(e) => {
                    return Err(err!(BaseAppErr::ComponentStartFailed, {
                        "index": idx,
                        "type_name": type_name,
                        "name": name,
                    })
                    .with_source(e));
                }
            }
        }

        tracing::info!(
            "All {} components started successfully",
            self.components.len()
        );
        Ok(())
    }

    /// 第二阶段：按启动顺序调 Deferred 组件的 `activate`。所有组件第一阶段都成功了才会走到这里，
    /// 失败时由 run 统一关闭全部组件
    async fn activate_components(&mut self) -> Result<(), HyErr> {
        for idx in 0..self.components.len() {
            let entry = &mut self.components[idx];
            if !entry.deferred {
                continue;
            }
            let (name, type_name) = (entry.name.clone(), entry.type_name);
            match entry
                .component
                .activate(&self.state, entry.shutdown.clone())
                .await
            {
                Ok(handle) => {
                    if handle.is_some() {
                        tracing::info!("{type_name}({name}) activated with background task");
                    } else {
                        tracing::info!("{type_name}({name}) activated");
                    }
                    entry.handle = handle;
                }
                Err(e) => {
                    return Err(err!(BaseAppErr::ComponentActivateFailed, {
                        "index": idx,
                        "type_name": type_name,
                        "name": name,
                    })
                    .with_source(e));
                }
            }
        }
        Ok(())
    }

    /// 等任意一个组件的后台任务结束，返回它的下标和结果。没有后台任务时永远等不到。
    /// 结束的任务从 entry 里取走（结束的 JoinHandle 不能再 poll）
    async fn any_task_exited(&mut self) -> (usize, Result<(), tokio::task::JoinError>) {
        let started = self.started;
        std::future::poll_fn(|cx| {
            for (idx, entry) in self.components[..started].iter_mut().enumerate() {
                if let Some(handle) = entry.handle.as_mut()
                    && let std::task::Poll::Ready(result) = std::pin::Pin::new(handle).poll(cx)
                {
                    entry.handle = None;
                    return std::task::Poll::Ready((idx, result));
                }
            }
            std::task::Poll::Pending
        })
        .await
    }

    fn task_exited_error(&self, idx: usize, result: Result<(), tokio::task::JoinError>) -> HyErr {
        let entry = &self.components[idx];
        let reason = match &result {
            Ok(()) => "exited",
            Err(e) if e.is_panic() => "panicked",
            Err(_) => "was cancelled",
        };
        let err = err!(BaseAppErr::TaskExited, {
            "type_name": entry.type_name,
            "name": entry.name,
            "reason": reason,
        });
        match result {
            Ok(()) => err,
            Err(e) => err.with_source(e),
        }
    }

    /// 组件关闭时实际用的超时：组件自己指定的，没有就用全局默认
    fn shutdown_timeout_of(&self, entry: &ComponentEntry) -> Duration {
        entry.shutdown_timeout.unwrap_or(self.shutdown_timeout)
    }

    /// 已启动组件的关闭顺序（下标）：先按启动的逆序关 Deferred，再按启动的逆序关 Immediate。
    /// Deferred 组件（HTTP / gRPC 等）先停止接活，它们用到的连接池这时都还在
    fn shutdown_order(&self) -> Vec<usize> {
        let (deferred, immediate): (Vec<usize>, Vec<usize>) = (0..self.started)
            .rev()
            .partition(|&idx| self.components[idx].deferred);
        deferred.into_iter().chain(immediate).collect()
    }

    /// 打出最长关闭时间，部署时照着配 k8s 的 terminationGracePeriodSeconds。
    /// 每个已启动的组件都算上它的超时（等后台任务 + `stop` 都在这个时间里）
    fn log_max_shutdown_time(&self) {
        let parts: Vec<(String, Duration)> = self
            .shutdown_order()
            .into_iter()
            .map(|idx| &self.components[idx])
            .map(|entry| {
                let label = format!("{}({})", entry.type_name, entry.name);
                (label, self.shutdown_timeout_of(entry))
            })
            .collect();
        let total =
            self.shutdown_delay + parts.iter().map(|(_, timeout)| *timeout).sum::<Duration>();
        let mut detail: Vec<String> = Vec::new();
        if !self.shutdown_delay.is_zero() {
            detail.push(format!("delay {:?}", self.shutdown_delay));
        }
        detail.extend(
            parts
                .iter()
                .map(|(label, timeout)| format!("{label} {timeout:?}")),
        );
        tracing::info!(
            "Max shutdown time: {total:?} ({}); k8s terminationGracePeriodSeconds should be larger",
            if detail.is_empty() {
                "nothing to wait".to_string()
            } else {
                detail.join(" + ")
            }
        );
    }

    /// 关闭已启动的组件；关闭过程中再收到一次信号就直接退出进程，见 [`force_exit`]
    async fn shutdown_or_force_exit(&mut self, signals: &mut ShutdownSignals) {
        tokio::select! {
            () = self.shutdown() => {}
            signal = signals.recv() => force_exit(signal),
        }
    }

    /// 按 [`shutdown_order`](Self::shutdown_order) 逐个关闭已启动的组件：
    /// 触发它的退出信号 → 等它的后台任务结束 → 调它的 `stop` → 下一个。
    ///
    /// 先停 Deferred 组件（比如 HTTP），再按启动的逆序停 Immediate 组件，被依赖的（比如数据库）后停，
    /// HTTP 收尾时数据库还在。
    /// 每个组件单独计时（等任务 + `stop` 共用），这样一个卡住的组件不会占掉后面组件（往往是数据库）的
    /// 收尾时间；超时的 abort 它的任务、打 WARN，接着关下一个，不会卡死
    async fn shutdown(&mut self) {
        let order = self.shutdown_order();
        self.started = 0;
        for idx in order {
            let timeout = self.shutdown_timeout_of(&self.components[idx]);
            let entry = &mut self.components[idx];
            entry.shutdown.cancel();
            let label = format!("{}({})", entry.type_name, entry.name);
            let mut handle = entry.handle.take();
            let component = &mut entry.component;
            let stopping = async {
                if let Some(handle) = handle.as_mut() {
                    match handle.await {
                        Ok(()) => {}
                        Err(e) if e.is_panic() => {
                            tracing::warn!("{label} background task panicked: {e}")
                        }
                        Err(e) => tracing::warn!("{label} background task cancelled: {e}"),
                    }
                }
                if let Err(e) = component.stop().await {
                    tracing::warn!("{label} stop failed: {e:#}");
                }
            };
            match tokio::time::timeout(timeout, stopping).await {
                Ok(()) => tracing::info!("{label} stopped"),
                Err(_) => {
                    if let Some(handle) = handle {
                        handle.abort();
                    }
                    tracing::warn!("{label} did not stop within {timeout:?}, aborted");
                }
            }
        }
    }
}

/// 在没排上的组件（`pending_deps > 0`）里找一个环，返回环上的组件下标，首尾相同，按依赖方向排
/// （`a -> b` 表示 a 要在 b 之前启动）。`after[a]` 是依赖 a 的组件
fn find_cycle(after: &[Vec<usize>], pending_deps: &[usize]) -> Vec<usize> {
    let stuck = |idx: usize| pending_deps[idx] > 0;
    // before[b]：b 依赖的、同样没排上的组件
    let mut before: Vec<Vec<usize>> = vec![Vec::new(); after.len()];
    for (a, nexts) in after.iter().enumerate() {
        for &b in nexts {
            if stuck(a) && stuck(b) {
                before[b].push(a);
            }
        }
    }
    let Some(start) = (0..after.len()).find(|&idx| stuck(idx)) else {
        return Vec::new();
    };
    // 沿着"依赖谁"一直往回走，一定会走回到路径上已经出现过的组件
    let mut path = vec![start];
    let mut current = start;
    loop {
        let Some(&prev) = before[current].first() else {
            return path;
        };
        if let Some(pos) = path.iter().position(|&idx| idx == prev) {
            let mut cycle = path[pos..].to_vec();
            cycle.push(prev);
            // path 是逆着依赖方向走的，反过来就是启动先后的方向
            cycle.reverse();
            return cycle;
        }
        path.push(prev);
        current = prev;
    }
}

#[cfg(test)]
mod tests {
    use super::find_cycle;

    // 只测 find_cycle 这个纯函数：给定 after / pending_deps（sort_by_dependencies 排不完时的中间状态），
    // 断言找到的环。不建 Registry：会装全局日志，占用 log.rs 的 INSTALLED 标记

    #[test]
    fn self_loop() {
        // 组件 0 依赖自己：after[0] 里有它自己，pending_deps[0] 永远排不掉
        let after = vec![vec![0]];
        let pending_deps = vec![1];
        assert_eq!(find_cycle(&after, &pending_deps), vec![0, 0]);
    }

    #[test]
    fn three_components_cycle() {
        // A(0) 依赖 B(1)，B(1) 依赖 C(2)，C(2) 依赖 A(0)：after[dep] 是依赖它的组件
        let after = vec![
            vec![2], // after[A] = [C]：C 依赖 A
            vec![0], // after[B] = [A]：A 依赖 B
            vec![1], // after[C] = [B]：B 依赖 C
        ];
        let pending_deps = vec![1, 1, 1];
        // 启动方向：A -> C -> B -> A
        assert_eq!(find_cycle(&after, &pending_deps), vec![0, 2, 1, 0]);
    }

    #[test]
    fn cycle_with_extra_component_outside() {
        // D(0) 不在环里、已经排好（pending_deps[0] == 0）；A(1)/B(2)/C(3) 和上面一样成环
        let after = vec![
            vec![],  // D 没人依赖
            vec![3], // after[A] = [C]：C 依赖 A
            vec![1], // after[B] = [A]：A 依赖 B
            vec![2], // after[C] = [B]：B 依赖 C
        ];
        let pending_deps = vec![0, 1, 1, 1];
        // D 不出现在结果里；启动方向：A -> C -> B -> A
        assert_eq!(find_cycle(&after, &pending_deps), vec![1, 3, 2, 1]);
    }
}
