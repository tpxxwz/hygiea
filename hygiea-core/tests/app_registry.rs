//! Registry 的组件注册、依赖排序、启动、运行、关闭。
//!
//! 进程里第一个 Registry 会真正装上全局日志，之后创建的 Registry 都走 `InstallFailed` 静默沿用的分支
//! （见 `app_registry_log.rs`），所以这里的多个测试可以放进同一个文件、同一个进程。
//!
//! 每个测试自己建一个 [`TestComponent`]，把 startup/stop 之类的事件按发生顺序记进共享的
//! `Arc<Mutex<Vec<String>>>`（`"{name}:startup"`、`"{name}:stop"` 这种格式），用这份记录断言
//! 启动顺序、关闭顺序、哪些组件被调用过。

#![cfg(feature = "app")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hygiea_core::app::{
    AppErr, CancellationToken, DeferredComponent, ImmediateComponent, Name, ReadyResources,
    Registry, RegistryConfig, ResourceId, ResourceSink, Resources, component,
};
use hygiea_core::{Result, err};
use tokio::task::JoinHandle;

// ---- 测试用组件 -------------------------------------------------------------

/// 共享的事件记录：每个组件的 startup / stop / 后台任务在做什么，按发生顺序追加
type EventLog = Arc<Mutex<Vec<String>>>;

fn events() -> EventLog {
    Arc::new(Mutex::new(Vec::new()))
}

fn snapshot(events: &EventLog) -> Vec<String> {
    events.lock().unwrap().clone()
}

/// 组件后台任务的行为
#[derive(Clone)]
enum TaskKind {
    /// 等到 shutdown 信号后再睡一段时间才结束，用来测关闭超时
    WaitShutdownThenSleep(Duration),
    /// 不等信号，启动后立刻结束，模拟任务提前退出
    ExitImmediately,
    /// 不等信号，启动后立刻 panic
    Panic,
}

/// 一个 [`TestComponent`] 的行为，用建造器风格拼装
#[derive(Clone)]
struct Behavior {
    provides: Vec<String>,
    depends_on: Vec<String>,
    /// 是否真的把 `provides` 声明的资源插入 Resources；false 用来模拟"声明了却没插入"
    insert_provides: bool,
    /// startup 直接返回错误
    fail_startup: bool,
    task: Option<TaskKind>,
    /// stop 返回 Err（仍然会记录 stop 事件）
    stop_err: bool,
    shutdown_timeout: Option<Duration>,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            provides: Vec::new(),
            depends_on: Vec::new(),
            insert_provides: true,
            fail_startup: false,
            task: None,
            stop_err: false,
            shutdown_timeout: None,
        }
    }
}

impl Behavior {
    fn provides(mut self, names: impl IntoIterator<Item = &'static str>) -> Self {
        self.provides = names.into_iter().map(str::to_string).collect();
        self
    }

    fn depends_on(mut self, names: impl IntoIterator<Item = &'static str>) -> Self {
        self.depends_on = names.into_iter().map(str::to_string).collect();
        self
    }

    fn no_insert(mut self) -> Self {
        self.insert_provides = false;
        self
    }

    fn fail_startup(mut self) -> Self {
        self.fail_startup = true;
        self
    }

    fn with_task(mut self, task: TaskKind) -> Self {
        self.task = Some(task);
        self
    }

    fn stop_err(mut self) -> Self {
        self.stop_err = true;
        self
    }

    fn shutdown_timeout(mut self, d: Duration) -> Self {
        self.shutdown_timeout = Some(d);
        self
    }
}

fn behavior() -> Behavior {
    Behavior::default()
}

struct TestConfig {
    behavior: Behavior,
    events: EventLog,
}

fn config(events: &EventLog, behavior: Behavior) -> TestConfig {
    TestConfig {
        behavior,
        events: events.clone(),
    }
}

struct TestComponent {
    name: Name,
    behavior: Behavior,
    events: EventLog,
}

#[component]
impl ImmediateComponent for TestComponent {
    type Config = TestConfig;

    fn build(name: Name, config: Self::Config) -> Self {
        Self {
            name,
            behavior: config.behavior,
            events: config.events,
        }
    }

    fn provides(&self) -> Vec<ResourceId> {
        self.behavior
            .provides
            .iter()
            .map(|n| ResourceId::named::<String>(n.clone()))
            .collect()
    }

    fn depends_on(&self) -> Vec<ResourceId> {
        self.behavior
            .depends_on
            .iter()
            .map(|n| ResourceId::named::<String>(n.clone()))
            .collect()
    }

    fn shutdown_timeout(&self) -> Option<Duration> {
        self.behavior.shutdown_timeout
    }

    async fn startup(
        &mut self,
        state: &Resources,
        shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:startup", self.name));

        if self.behavior.fail_startup {
            return Err(err!(
                AppErr::ComponentError,
                "test startup failure".to_string()
            ));
        }

        if self.behavior.insert_provides {
            for name in &self.behavior.provides {
                state.insert_named::<String>(name.clone(), format!("value-of-{name}"));
            }
        }

        let handle = match self.behavior.task.clone() {
            None => None,
            Some(task) => {
                let events = self.events.clone();
                let name = self.name.clone();
                Some(tokio::spawn(async move {
                    match task {
                        TaskKind::WaitShutdownThenSleep(dur) => {
                            shutdown.cancelled().await;
                            tokio::time::sleep(dur).await;
                            events.lock().unwrap().push(format!("{name}:task_done"));
                        }
                        TaskKind::ExitImmediately => {
                            events.lock().unwrap().push(format!("{name}:task_exit"));
                        }
                        TaskKind::Panic => {
                            panic!("test background task panic");
                        }
                    }
                }))
            }
        };
        Ok(handle)
    }

    async fn stop(&mut self) -> Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:stop", self.name));
        if self.behavior.stop_err {
            return Err(err!(
                AppErr::ComponentError,
                "test stop failure".to_string()
            ));
        }
        Ok(())
    }
}

/// `on_ready` 的回调故意返回错误，让 `run` 在启动完成后立刻进入关闭流程，不用等 OS 信号
async fn force_shutdown(_state: Resources) -> Result<()> {
    Err(err!(
        AppErr::ComponentError,
        "forced shutdown for test".to_string()
    ))
}

fn registry_with(shutdown_timeout_secs: u64) -> Registry {
    Registry::with_config(RegistryConfig {
        shutdown_timeout_secs,
        ..RegistryConfig::default()
    })
}

// ---- 1. 依赖排序 + 无依赖时保持 add 顺序 -------------------------------------

#[tokio::test]
async fn dependency_order_and_add_order_preserved() {
    let events = events();
    // add 顺序：a、consumer（依赖 "dep"）、b、provider（提供 "dep"）
    // 期望启动顺序：a、b 之间没有依赖关系，保持 add 顺序；provider 提供 consumer 依赖的资源，必须排在它前面
    let registry = registry_with(15)
        .add_named::<TestComponent>("a", config(&events, behavior()))
        .add_named::<TestComponent>("consumer", config(&events, behavior().depends_on(["dep"])))
        .add_named::<TestComponent>("b", config(&events, behavior()))
        .add_named::<TestComponent>("provider", config(&events, behavior().provides(["dep"])));

    let (result, _guard) = registry.on_ready(force_shutdown).run().await;
    assert!(result.is_err());

    let startups: Vec<String> = snapshot(&events)
        .into_iter()
        .filter(|e| e.ends_with(":startup"))
        .map(|e| e.trim_end_matches(":startup").to_string())
        .collect();
    assert_eq!(startups, vec!["a", "b", "provider", "consumer"]);
}

// ---- 2. 启动前的依赖检查：缺依赖 / 重复提供 / 成环，都不调用任何 startup -----

#[tokio::test]
async fn missing_dependency_reports_resource_missing_before_any_startup() {
    let events = events();
    let registry = registry_with(15).add_named::<TestComponent>(
        "consumer",
        config(&events, behavior().depends_on(["missing-resource"])),
    );

    let (result, _guard) = registry
        .on_ready(|_state| async {
            panic!("依赖检查失败前不应该进到 on_ready 的回调")
        })
        .run()
        .await;

    let err = result.unwrap_err();
    assert!(err.is(AppErr::ResourceMissing));
    assert!(format!("{err:#}").contains("missing-resource"));
    assert!(snapshot(&events).is_empty());
}

#[tokio::test]
async fn duplicate_provider_reports_duplicate_provider_before_any_startup() {
    let events = events();
    let registry = registry_with(15)
        .add_named::<TestComponent>("p1", config(&events, behavior().provides(["dup"])))
        .add_named::<TestComponent>("p2", config(&events, behavior().provides(["dup"])));

    let (result, _guard) = registry
        .on_ready(|_state| async {
            panic!("依赖检查失败前不应该进到 on_ready 的回调")
        })
        .run()
        .await;

    let err = result.unwrap_err();
    assert!(err.is(AppErr::DuplicateProvider));
    let msg = format!("{err:#}");
    assert!(msg.contains("p1") && msg.contains("p2"), "{msg}");
    assert!(snapshot(&events).is_empty());
}

#[tokio::test]
async fn dependency_cycle_reports_cycle_path_before_any_startup() {
    let events = events();
    let registry = registry_with(15)
        .add_named::<TestComponent>(
            "a",
            config(&events, behavior().provides(["a"]).depends_on(["b"])),
        )
        .add_named::<TestComponent>(
            "b",
            config(&events, behavior().provides(["b"]).depends_on(["a"])),
        );

    let (result, _guard) = registry
        .on_ready(|_state| async {
            panic!("依赖检查失败前不应该进到 on_ready 的回调")
        })
        .run()
        .await;

    let err = result.unwrap_err();
    assert!(err.is(AppErr::DependencyCycle));
    let msg = format!("{err:#}");
    assert!(
        msg.contains("a") && msg.contains("b") && msg.contains("->"),
        "{msg}"
    );
    assert!(snapshot(&events).is_empty());
}

// ---- 3. 启动失败：已启动组件逆序 stop，返回 ComponentStartFailed -----------

#[tokio::test]
async fn start_failure_stops_already_started_components_in_reverse() {
    let events = events();
    let registry = registry_with(15)
        .add_named::<TestComponent>("ok1", config(&events, behavior()))
        .add_named::<TestComponent>("ok2", config(&events, behavior()))
        .add_named::<TestComponent>("bad", config(&events, behavior().fail_startup()));

    let (result, _guard) = registry
        .on_ready(|_state| async {
            panic!("组件启动失败，不应该进到 on_ready 的回调")
        })
        .run()
        .await;

    let err = result.unwrap_err();
    assert!(err.is(AppErr::ComponentStartFailed));
    // bad 自己的 startup 返回 Err 这条路径不会把它标记为"已启动"，所以它自己不会被 stop
    assert_eq!(
        snapshot(&events),
        vec![
            "ok1:startup",
            "ok2:startup",
            "bad:startup",
            "ok2:stop",
            "ok1:stop"
        ]
    );
}

// ---- 4. 声明了 provides 但没有插入：报错，且这个组件自己也被 stop ----------

#[tokio::test]
async fn declared_but_not_inserted_resource_fails_and_stops_itself() {
    let events = events();
    let registry = registry_with(15).add_named::<TestComponent>(
        "ghost",
        config(&events, behavior().provides(["ghost-res"]).no_insert()),
    );

    let (result, _guard) = registry
        .on_ready(|_state| async {
            panic!("组件启动失败，不应该进到 on_ready 的回调")
        })
        .run()
        .await;

    let err = result.unwrap_err();
    assert!(err.is(AppErr::ComponentStartFailed));
    assert!(format!("{err:#}").contains("ghost-res"));
    // 和 start_failure 那条不同：这里 startup 本身返回了 Ok，只是校验 provides 时发现缺失，
    // 所以这个组件已经被记为"已启动"，会被 stop
    assert_eq!(snapshot(&events), vec!["ghost:startup", "ghost:stop"]);
}

// ---- 5. on_ready 的回调返回 Err：所有组件逆序关闭，返回同一个错误 --------------

#[tokio::test]
async fn app_init_failure_shuts_down_all_components_in_reverse() {
    let events = events();
    let registry = registry_with(15)
        .add_named::<TestComponent>("a", config(&events, behavior()))
        .add_named::<TestComponent>("b", config(&events, behavior()));

    let (result, _guard) = registry.on_ready(force_shutdown).run().await;

    let err = result.unwrap_err();
    assert!(err.is(AppErr::ComponentError));
    assert_eq!(
        snapshot(&events),
        vec!["a:startup", "b:startup", "b:stop", "a:stop"]
    );
}

// ---- 6. 后台任务提前退出 / panic：TaskExited，并关闭所有组件 ---------------

#[tokio::test]
async fn background_task_exit_reports_task_exited_and_shuts_down() {
    let events = events();
    let registry = registry_with(15)
        .add_named::<TestComponent>(
            "worker",
            config(&events, behavior().with_task(TaskKind::ExitImmediately)),
        )
        .add_named::<TestComponent>("other", config(&events, behavior()));

    let (result, _guard) = registry
        .on_ready(|_state| async {
            // 主动让出一次调度，确保刚 spawn 的后台任务有机会先跑完（它自己不等待任何东西），
            // 这样 f 返回之后进入的 any_task_exited 才能观察到"已经退出"
            tokio::task::yield_now().await;
            Ok(())
        })
        .run()
        .await;

    let err = result.unwrap_err();
    assert!(err.is(AppErr::TaskExited));
    assert_eq!(
        snapshot(&events),
        vec![
            "worker:startup",
            "other:startup",
            "worker:task_exit",
            "other:stop",
            "worker:stop"
        ]
    );
}

#[tokio::test]
async fn background_task_panic_reports_task_exited_and_shuts_down() {
    let events = events();
    let registry = registry_with(15)
        .add_named::<TestComponent>(
            "worker",
            config(&events, behavior().with_task(TaskKind::Panic)),
        )
        .add_named::<TestComponent>("other", config(&events, behavior()));

    let (result, _guard) = registry
        .on_ready(|_state| async {
            tokio::task::yield_now().await;
            Ok(())
        })
        .run()
        .await;

    let err = result.unwrap_err();
    assert!(err.is(AppErr::TaskExited));
    assert_eq!(
        snapshot(&events),
        vec![
            "worker:startup",
            "other:startup",
            "other:stop",
            "worker:stop"
        ]
    );
}

// ---- 7. stop 超时的组件被 abort，下一个组件照常关闭 ------------------------

#[tokio::test]
async fn slow_stop_is_aborted_and_next_component_still_stops() {
    let events = events();
    // 全局超时给得很短：slow 的后台任务在收到 shutdown 信号后要睡 2 秒才结束，肯定会超时被 abort
    let registry = registry_with(1)
        .add_named::<TestComponent>("normal", config(&events, behavior()))
        .add_named::<TestComponent>(
            "slow",
            config(
                &events,
                behavior().with_task(TaskKind::WaitShutdownThenSleep(Duration::from_secs(2))),
            ),
        );

    let started = std::time::Instant::now();
    let (result, _guard) = registry.on_ready(force_shutdown).run().await;
    let elapsed = started.elapsed();

    assert!(result.is_err());
    // 超时是 1 秒，不应该真的等满 2 秒的 sleep
    assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
    // slow 等 handle 超时：stopping 这个 future 整体被 timeout 打断，component.stop() 根本没机会被调用；
    // 关闭继续处理 normal，它正常 stop
    assert_eq!(
        snapshot(&events),
        vec!["normal:startup", "slow:startup", "normal:stop"]
    );
}

// ---- 8. 组件自己的 shutdown_timeout 优先于全局配置 -------------------------

#[tokio::test]
async fn component_shutdown_timeout_overrides_global_default() {
    let events = events();
    // 全局给得很长（3 秒），slow 自己声明只等 50ms；后台任务要睡 300ms 才能正常结束（远超 50ms）。
    // 如果 override 没生效、用了全局的 3 秒，任务会在 300ms 时正常结束，看得到 task_done / stop；
    // override 生效的话，会在 ~50ms 时被 abort，两者都看不到
    let registry = registry_with(3)
        .add_named::<TestComponent>(
            "slow",
            config(
                &events,
                behavior()
                    .with_task(TaskKind::WaitShutdownThenSleep(Duration::from_millis(300)))
                    .shutdown_timeout(Duration::from_millis(50)),
            ),
        )
        .add_named::<TestComponent>("normal", config(&events, behavior()));

    let started = std::time::Instant::now();
    let (result, _guard) = registry.on_ready(force_shutdown).run().await;
    let elapsed = started.elapsed();

    assert!(result.is_err());
    assert!(elapsed < Duration::from_millis(1000), "{elapsed:?}");
    let log = snapshot(&events);
    assert!(!log.contains(&"slow:task_done".to_string()), "{log:?}");
    assert!(!log.contains(&"slow:stop".to_string()), "{log:?}");
    // normal 是先启动的（后关闭），照常被 stop
    assert!(log.contains(&"normal:stop".to_string()), "{log:?}");
}

// ---- 9. stop 返回 Err 只打 WARN，不影响后面组件 ----------------------------

#[tokio::test]
async fn stop_error_only_warns_and_does_not_block_later_components() {
    let events = events();
    // add 顺序 b、a：启动顺序也是 b、a（没有依赖关系），关闭逆序是 a 先、b 后，
    // 这样 a 的 stop 失败之后还有 b 会继续关闭
    let registry = registry_with(15)
        .add_named::<TestComponent>("b", config(&events, behavior()))
        .add_named::<TestComponent>("a", config(&events, behavior().stop_err()));

    let (result, _guard) = registry.on_ready(force_shutdown).run().await;

    let err = result.unwrap_err();
    // run 的结果是回调的错误，不会变成 StopFailed：stop 失败只打 WARN
    assert!(err.is(AppErr::ComponentError));
    assert_eq!(
        snapshot(&events),
        vec!["b:startup", "a:startup", "a:stop", "b:stop"]
    );
}

// ---- 10. add_named 同类型同名重复 panic，不同名可以共存 -------------------

#[test]
fn add_named_duplicate_name_panics() {
    let events = events();
    let registry = registry_with(15).add_named::<TestComponent>("dup", config(&events, behavior()));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        registry.add_named::<TestComponent>("dup", config(&events, behavior()))
    }));
    assert!(result.is_err());
}

#[test]
fn add_named_different_names_coexist() {
    let events = events();
    // 不 panic 就是通过
    let _registry = registry_with(15)
        .add_named::<TestComponent>("x", config(&events, behavior()))
        .add_named::<TestComponent>("y", config(&events, behavior()));
}

// ---- 11. Deferred 组件：两阶段启动、依赖不参与排序、关闭时先关 --------------------

/// Deferred 测试组件的行为
#[derive(Clone, Default)]
struct DeferredBehavior {
    /// prepare 里放进去的资源
    provides: Vec<String>,
    /// 声明的依赖，activate 里读
    depends_on: Vec<String>,
    /// 不声明、activate 里直接读的资源：`before_activate` 放进去的资源没有组件提供，只能这样读
    undeclared_reads: Vec<String>,
    /// activate 直接返回错误
    fail_activate: bool,
    /// activate 返回一个立刻结束的后台任务，让 run 以 TaskExited 进入关闭，不用等 OS 信号
    exit_after_activate: bool,
}

struct DeferredConfig {
    behavior: DeferredBehavior,
    events: EventLog,
}

/// Deferred 组件：prepare / activate / stop 按 `"{name}:prepare"` 这种格式记事件，
/// activate 时把读到的依赖也记下来
struct TestDeferred {
    name: Name,
    behavior: DeferredBehavior,
    events: EventLog,
}

#[component]
impl DeferredComponent for TestDeferred {
    type Config = DeferredConfig;

    fn build(name: Name, config: Self::Config) -> Self {
        Self {
            name,
            behavior: config.behavior,
            events: config.events,
        }
    }

    fn provides(&self) -> Vec<ResourceId> {
        self.behavior
            .provides
            .iter()
            .map(|n| ResourceId::named::<String>(n.clone()))
            .collect()
    }

    fn depends_on(&self) -> Vec<ResourceId> {
        self.behavior
            .depends_on
            .iter()
            .map(|n| ResourceId::named::<String>(n.clone()))
            .collect()
    }

    async fn prepare(&mut self, sink: &ResourceSink) -> Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:prepare", self.name));
        for name in &self.behavior.provides {
            sink.insert_named::<String>(name.clone(), format!("value-of-{name}"));
        }
        Ok(())
    }

    async fn activate(
        &mut self,
        resources: ReadyResources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        let mut read = Vec::new();
        for name in self
            .behavior
            .depends_on
            .iter()
            .chain(&self.behavior.undeclared_reads)
        {
            read.push(resources.require_named::<String>(name)?);
        }
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:activate[{}]", self.name, read.join(",")));
        if self.behavior.fail_activate {
            return Err(err!(
                AppErr::ComponentError,
                "test activate failure".to_string()
            ));
        }
        if !self.behavior.exit_after_activate {
            return Ok(None);
        }
        let events = self.events.clone();
        let name = self.name.clone();
        Ok(Some(tokio::spawn(async move {
            events.lock().unwrap().push(format!("{name}:task_exit"));
        })))
    }

    async fn stop(&mut self) -> Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:stop", self.name));
        Ok(())
    }
}

fn deferred_config(events: &EventLog, behavior: DeferredBehavior) -> DeferredConfig {
    DeferredConfig {
        behavior,
        events: events.clone(),
    }
}

#[tokio::test]
async fn deferred_activates_between_before_activate_and_run() {
    let events = events();
    let hook_events = events.clone();
    let run_events = events.clone();
    // api 先 add：它的依赖不参与第一阶段排序，所以最先 prepare；
    // activate 等 db 启动完、before_activate 执行完；on_ready 在 activate 之后
    let registry = registry_with(15)
        .add_named::<TestDeferred>(
            "api",
            deferred_config(
                &events,
                DeferredBehavior {
                    provides: vec!["addr".into()],
                    depends_on: vec!["db".into()],
                    undeclared_reads: vec!["hooked".into()],
                    ..Default::default()
                },
            ),
        )
        .add_named::<TestComponent>("db", config(&events, behavior().provides(["db"])))
        .before_activate(move |state| async move {
            // 第一阶段的资源都在了（包括 Deferred 在 prepare 里放的），api 还没 activate
            state.require_named::<String>("db")?;
            state.require_named::<String>("addr")?;
            state.insert_named::<String>("hooked", "value-of-hooked".to_string());
            hook_events
                .lock()
                .unwrap()
                .push("before_activate".to_string());
            Ok(())
        });

    let (result, _guard) = registry
        .on_ready(|state| async move {
            run_events.lock().unwrap().push("run".to_string());
            force_shutdown(state).await
        })
        .run()
        .await;
    assert!(result.is_err());

    assert_eq!(
        snapshot(&events),
        [
            "api:prepare",
            "db:startup",
            "before_activate",
            // before_activate 里放进去的资源 activate 也能读到
            "api:activate[value-of-db,value-of-hooked]",
            "run",
            "api:stop",
            "db:stop",
        ]
    );
}

#[tokio::test]
async fn before_activate_failure_stops_all_without_activate() {
    let events = events();
    let registry = registry_with(15)
        .add_named::<TestComponent>("db", config(&events, behavior()))
        .add_named::<TestDeferred>("api", deferred_config(&events, DeferredBehavior::default()))
        .before_activate(|_state| async {
            Err(err!(
                AppErr::ComponentError,
                "test before_activate failure".to_string()
            ))
        });

    let (result, _guard) = registry
        .on_ready(|_state| async {
            panic!("before_activate 失败，不应该进到 on_ready 的回调")
        })
        .run()
        .await;
    let err = result.expect_err("before_activate 失败应该返回它的错误");
    assert!(err.is(AppErr::ComponentError), "{err:#}");
    // api 没有 activate，但 prepare 过，照样要关
    assert_eq!(
        snapshot(&events),
        ["db:startup", "api:prepare", "api:stop", "db:stop"]
    );
}

#[test]
#[should_panic(expected = "before_activate already set")]
fn before_activate_twice_panics() {
    let _ = registry_with(15)
        .before_activate(|_| async { Ok(()) })
        .before_activate(|_| async { Ok(()) });
}

#[tokio::test]
async fn deferred_dependency_does_not_form_startup_cycle() {
    let events = events();
    // api（Deferred）依赖 cache，cache 又依赖 api 在 prepare 里放的 addr。
    // 按阶段看能跑：api 的依赖只在 activate 里读，不约束第一阶段，所以不算环
    let registry = registry_with(15)
        .add_named::<TestComponent>(
            "cache",
            config(&events, behavior().provides(["cache"]).depends_on(["addr"])),
        )
        .add_named::<TestDeferred>(
            "api",
            deferred_config(
                &events,
                DeferredBehavior {
                    provides: vec!["addr".into()],
                    depends_on: vec!["cache".into()],
                    exit_after_activate: true,
                    ..Default::default()
                },
            ),
        );

    let (result, _guard) = registry.run().await;
    let err = result.expect_err("api 的后台任务结束，应该以 TaskExited 返回");
    assert!(err.is(AppErr::TaskExited), "{err:#}");
    assert_eq!(
        snapshot(&events),
        [
            "api:prepare",
            "cache:startup",
            "api:activate[value-of-cache]",
            "api:task_exit",
            "api:stop",
            "cache:stop",
        ]
    );
}

#[tokio::test]
async fn deferred_missing_dependency_reports_before_any_startup() {
    let events = events();
    let registry = registry_with(15).add_named::<TestDeferred>(
        "api",
        deferred_config(
            &events,
            DeferredBehavior {
                depends_on: vec!["nobody".into()],
                ..Default::default()
            },
        ),
    );

    let (result, _guard) = registry
        .on_ready(|_state| async {
            panic!("依赖检查失败前不应该进到 on_ready 的回调")
        })
        .run()
        .await;
    let err = result.expect_err("Deferred 的依赖没有提供者，应该启动前就失败");
    assert!(err.is(AppErr::ResourceMissing), "{err:#}");
    assert!(snapshot(&events).is_empty());
}

#[tokio::test]
async fn activate_failure_shuts_down_deferred_first_then_immediate_in_reverse() {
    let events = events();
    // 启动顺序 a、web、b（b 依赖 a）；关闭时先关 Deferred 的 web，再逆序关 b、a
    let registry = registry_with(15)
        .add_named::<TestComponent>("a", config(&events, behavior().provides(["a"])))
        .add_named::<TestDeferred>(
            "web",
            deferred_config(
                &events,
                DeferredBehavior {
                    fail_activate: true,
                    ..Default::default()
                },
            ),
        )
        .add_named::<TestComponent>("b", config(&events, behavior().depends_on(["a"])));

    let (result, _guard) = registry.run().await;
    let err = result.expect_err("activate 失败应该返回错误");
    assert!(err.is(AppErr::ComponentActivateFailed), "{err:#}");
    assert_eq!(
        snapshot(&events),
        [
            "a:startup",
            "web:prepare",
            "b:startup",
            "web:activate[]",
            "web:stop",
            "b:stop",
            "a:stop",
        ]
    );
}

#[test]
#[should_panic(expected = "on_ready already set")]
fn on_ready_twice_panics() {
    let _ = registry_with(15)
        .on_ready(|_| async { Ok(()) })
        .on_ready(|_| async { Ok(()) });
}
