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
    BaseAppErr, CancellationToken, Component, Name, Registry, RegistryConfig, ResourceId,
    Resources, async_trait,
};
use hygiea_core::{HyErr, err};
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

#[async_trait]
impl Component for TestComponent {
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
    ) -> Result<Option<JoinHandle<()>>, HyErr> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:startup", self.name));

        if self.behavior.fail_startup {
            return Err(err!(
                BaseAppErr::ComponentError,
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

    async fn stop(&mut self) -> Result<(), HyErr> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:stop", self.name));
        if self.behavior.stop_err {
            return Err(err!(
                BaseAppErr::ComponentError,
                "test stop failure".to_string()
            ));
        }
        Ok(())
    }
}

/// `run` 的回调故意返回错误，让 `run` 在启动完成后立刻进入关闭流程，不用等 OS 信号
async fn force_shutdown(_state: Resources) -> Result<(), HyErr> {
    Err(err!(
        BaseAppErr::ComponentError,
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

    let (result, _guard) = registry.run(force_shutdown).await;
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
        .run(|_state| async { panic!("依赖检查失败前不应该进到 run 的回调") })
        .await;

    let err = result.unwrap_err();
    assert!(err.is(BaseAppErr::ResourceMissing));
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
        .run(|_state| async { panic!("依赖检查失败前不应该进到 run 的回调") })
        .await;

    let err = result.unwrap_err();
    assert!(err.is(BaseAppErr::DuplicateProvider));
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
        .run(|_state| async { panic!("依赖检查失败前不应该进到 run 的回调") })
        .await;

    let err = result.unwrap_err();
    assert!(err.is(BaseAppErr::DependencyCycle));
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
        .run(|_state| async { panic!("组件启动失败，不应该进到 run 的回调") })
        .await;

    let err = result.unwrap_err();
    assert!(err.is(BaseAppErr::ComponentStartFailed));
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
        .run(|_state| async { panic!("组件启动失败，不应该进到 run 的回调") })
        .await;

    let err = result.unwrap_err();
    assert!(err.is(BaseAppErr::ComponentStartFailed));
    assert!(format!("{err:#}").contains("ghost-res"));
    // 和 start_failure 那条不同：这里 startup 本身返回了 Ok，只是校验 provides 时发现缺失，
    // 所以这个组件已经被记为"已启动"，会被 stop
    assert_eq!(snapshot(&events), vec!["ghost:startup", "ghost:stop"]);
}

// ---- 5. run 的回调返回 Err：所有组件逆序关闭，返回同一个错误 --------------

#[tokio::test]
async fn app_init_failure_shuts_down_all_components_in_reverse() {
    let events = events();
    let registry = registry_with(15)
        .add_named::<TestComponent>("a", config(&events, behavior()))
        .add_named::<TestComponent>("b", config(&events, behavior()));

    let (result, _guard) = registry.run(force_shutdown).await;

    let err = result.unwrap_err();
    assert!(err.is(BaseAppErr::ComponentError));
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
        .run(|_state| async {
            // 主动让出一次调度，确保刚 spawn 的后台任务有机会先跑完（它自己不等待任何东西），
            // 这样 f 返回之后进入的 any_task_exited 才能观察到"已经退出"
            tokio::task::yield_now().await;
            Ok(())
        })
        .await;

    let err = result.unwrap_err();
    assert!(err.is(BaseAppErr::TaskExited));
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
        .run(|_state| async {
            tokio::task::yield_now().await;
            Ok(())
        })
        .await;

    let err = result.unwrap_err();
    assert!(err.is(BaseAppErr::TaskExited));
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
    let (result, _guard) = registry.run(force_shutdown).await;
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
    let (result, _guard) = registry.run(force_shutdown).await;
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

    let (result, _guard) = registry.run(force_shutdown).await;

    let err = result.unwrap_err();
    // run 的结果是回调的错误，不会变成 StopFailed：stop 失败只打 WARN
    assert!(err.is(BaseAppErr::ComponentError));
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
