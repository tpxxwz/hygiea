//! `ImmediateResourceComponent` 经 Registry 启动：按组件名放进资源、关闭时调 `close`、建资源失败时启动失败。
//!
//! 进程里第一个 Registry 会真正装上全局日志，之后的走 `InstallFailed` 静默沿用（见 `app_registry_log.rs`），
//! 所以多个测试可以放在同一个文件里

#![cfg(feature = "app")]

use std::sync::{Arc, Mutex};

use hygiea_core::app::{
    AppErr, ConfigResource, ImmediateResourceComponent, Registry, ResourceId, Resources,
    async_trait,
};
use hygiea_core::{Result, err};

/// 按发生顺序记下 `from_config` / `close`
type EventLog = Arc<Mutex<Vec<String>>>;

#[derive(Clone)]
struct FakeConfig {
    value: String,
    fail: bool,
    events: EventLog,
}

/// 测试用资源：从配置里取出 `value`，关闭时记一条事件
#[derive(Clone, Debug)]
struct FakeResource {
    value: String,
    events: EventLog,
}

#[async_trait]
impl ConfigResource for FakeResource {
    type Config = FakeConfig;

    async fn from_config(config: &FakeConfig) -> Result<Self> {
        config
            .events
            .lock()
            .unwrap()
            .push(format!("{}:from_config", config.value));
        if config.fail {
            return Err(err!(AppErr::ConnectFailed, "test connect failure"));
        }
        Ok(Self {
            value: config.value.clone(),
            events: config.events.clone(),
        })
    }

    async fn close(&self) -> Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:close", self.value));
        Ok(())
    }
}

type FakeComponent = ImmediateResourceComponent<FakeResource>;

fn config(events: &EventLog, value: &str) -> FakeConfig {
    FakeConfig {
        value: value.to_string(),
        fail: false,
        events: events.clone(),
    }
}

fn snapshot(events: &EventLog) -> Vec<String> {
    events.lock().unwrap().clone()
}

/// `on_ready` 的回调返回错误，让 `run` 启动完成后立刻进入关闭流程
fn shutdown_err() -> hygiea_core::HyErr {
    err!(AppErr::ComponentError, "forced shutdown for test")
}

#[tokio::test]
async fn named_instances_are_inserted_and_closed_in_reverse() {
    let events = EventLog::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in_ready = seen.clone();
    let registry = Registry::new()
        .add_named::<FakeComponent>("primary", config(&events, "p"))
        .add_named::<FakeComponent>("replica", config(&events, "r"))
        .on_ready(move |res: Resources| async move {
            for name in ["primary", "replica"] {
                let r = res.require_named::<FakeResource>(name)?;
                seen_in_ready.lock().unwrap().push(r.value);
            }
            Err(shutdown_err())
        });

    let (result, _guard) = registry.run().await;
    assert!(result.unwrap_err().is(AppErr::ComponentError));
    assert_eq!(*seen.lock().unwrap(), vec!["p", "r"]);
    assert_eq!(
        snapshot(&events),
        vec!["p:from_config", "r:from_config", "r:close", "p:close"]
    );
}

#[tokio::test]
async fn anonymous_instance_is_got_without_name() {
    let events = EventLog::default();
    let registry = Registry::new()
        .add::<FakeComponent>(config(&events, "only"))
        .on_ready(|res: Resources| async move {
            assert_eq!(res.require::<FakeResource>()?.value, "only");
            Err(shutdown_err())
        });

    let (result, _guard) = registry.run().await;
    assert!(result.unwrap_err().is(AppErr::ComponentError));
    assert_eq!(snapshot(&events), vec!["only:from_config", "only:close"]);
}

/// 依赖方声明 `ResourceId::named::<R>(名字)`，Registry 把资源组件排在前面
#[tokio::test]
async fn provides_named_resource_for_dependency_ordering() {
    use hygiea_core::app::{CancellationToken, ImmediateComponent, Name, component};
    use tokio::task::JoinHandle;

    struct Consumer {
        events: EventLog,
    }

    #[component]
    impl ImmediateComponent for Consumer {
        type Config = EventLog;

        fn build(_name: Name, events: EventLog) -> Self {
            Self { events }
        }

        fn depends_on(&self) -> Vec<ResourceId> {
            vec![ResourceId::named::<FakeResource>("primary")]
        }

        async fn startup(
            &mut self,
            res: &Resources,
            _shutdown: CancellationToken,
        ) -> Result<Option<JoinHandle<()>>> {
            let r = res.require_named::<FakeResource>("primary")?;
            self.events
                .lock()
                .unwrap()
                .push(format!("consumer:got {}", r.value));
            Ok(None)
        }
    }

    let events = EventLog::default();
    let registry = Registry::new()
        .add::<Consumer>(events.clone())
        .add_named::<FakeComponent>("primary", config(&events, "p"))
        .on_ready(|_| async { Err(shutdown_err()) });

    let (result, _guard) = registry.run().await;
    assert!(result.is_err());
    assert_eq!(
        snapshot(&events),
        vec!["p:from_config", "consumer:got p", "p:close"]
    );
}

/// `from_config` 失败：启动失败，资源没建出来，不会调 `close`
#[tokio::test]
async fn from_config_failure_fails_startup() {
    let events = EventLog::default();
    let registry = Registry::new()
        .add_named::<FakeComponent>(
            "primary",
            FakeConfig {
                fail: true,
                ..config(&events, "p")
            },
        )
        .on_ready(|_| async { panic!("启动失败，不应该进到 on_ready 的回调") });

    let (result, _guard) = registry.run().await;
    let err = result.unwrap_err();
    assert!(err.is(AppErr::ComponentStartFailed), "{err:#}");
    assert!(format!("{err:#}").contains("test connect failure"), "{err:#}");
    assert_eq!(snapshot(&events), vec!["p:from_config"]);
}
