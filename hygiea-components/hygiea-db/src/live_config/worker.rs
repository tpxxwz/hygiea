//! 后台任务：独占一条连接，串行处理 bind、通知、轮询、重连

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;

use hygiea_core::app::CancellationToken;
use hygiea_core::{HyErr, Result};

use super::store::{Loaded, Signal, Store};

/// 重建 listener 失败后隔多久再试
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// 一个 namespace 当前生效的版本号和表里的值
pub(crate) struct NsState {
    pub(crate) revision: i64,
    pub(crate) values: Map<String, Value>,
}

/// 绑定到一个 struct 的若干 namespace。任务只看到类型擦除后的这一层
pub(crate) trait Binding: Send {
    fn namespaces(&self) -> &[String];
    /// 按当前各 namespace 的值重新拼出 struct 并替换；转换失败返回错误，旧值不动
    fn apply(&self, states: &HashMap<String, NsState>) -> Result<()>;
}

pub(crate) enum Command {
    Bind {
        /// 各 namespace 的默认值（第一层是 key）
        defaults: Vec<(String, Map<String, Value>)>,
        binding: Box<dyn Binding>,
        reply: oneshot::Sender<Result<()>>,
    },
}

pub(crate) struct Worker {
    pub(crate) poll_interval: Duration,
    /// PG 的 listener 报错后丢掉（`is_open` 为 false），主循环先重建
    pub(crate) store: Store,
    pub(crate) commands: mpsc::Receiver<Command>,
    pub(crate) states: HashMap<String, NsState>,
    pub(crate) bindings: Vec<Box<dyn Binding>>,
}

enum Event {
    Shutdown,
    Command(Command),
    /// 所有 LiveConfigSource 都 drop 了，不会再有 bind，已有的绑定照常更新
    CommandsClosed,
    Poll,
    Signal(Result<Signal>),
}

impl Worker {
    pub(crate) async fn run(mut self, shutdown: CancellationToken) {
        let mut poll = tokio::time::interval(self.poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // interval 第一次立刻触发，启动时刚加载过，跳过
        poll.reset();
        let mut commands_open = true;
        loop {
            if !self.store.is_open() {
                if !self.rebuild_listener(&shutdown).await {
                    break;
                }
                continue;
            }
            let store = &mut self.store;
            let event = tokio::select! {
                biased;
                _ = shutdown.cancelled() => Event::Shutdown,
                command = self.commands.recv(), if commands_open => match command {
                    Some(command) => Event::Command(command),
                    None => Event::CommandsClosed,
                },
                _ = poll.tick() => Event::Poll,
                signal = store.recv() => Event::Signal(signal),
            };
            match event {
                Event::Shutdown => break,
                Event::Command(Command::Bind {
                    defaults,
                    binding,
                    reply,
                }) => {
                    let result = self.bind(defaults, binding).await;
                    let _ = reply.send(result);
                }
                Event::CommandsClosed => commands_open = false,
                Event::Poll => self.sync_all().await,
                Event::Signal(Ok(Signal::Changed {
                    namespace,
                    revision,
                })) => {
                    let newer = self
                        .states
                        .get(&namespace)
                        .is_some_and(|state| revision > state.revision);
                    if newer {
                        self.reload(&namespace).await;
                    }
                }
                Event::Signal(Ok(Signal::Reconnected)) => {
                    tracing::info!(
                        "live config: connection re-established, checking all namespaces"
                    );
                    self.sync_all().await;
                }
                Event::Signal(Err(e)) => self.failed(&e),
            }
        }
        self.store.close().await;
    }

    /// 补齐缺的 key、读出各 namespace、拼一次 struct；都成功了才记下这个绑定
    async fn bind(
        &mut self,
        defaults: Vec<(String, Map<String, Value>)>,
        binding: Box<dyn Binding>,
    ) -> Result<()> {
        let seeded = self.store.seed(&defaults).await;
        self.check(seeded)?;
        for (namespace, _) in &defaults {
            let loaded = self.store.load(namespace).await;
            let loaded = self.check(loaded)?;
            self.update(namespace, loaded);
        }
        binding.apply(&self.states)?;
        self.bindings.push(binding);
        Ok(())
    }

    /// 重新读一个 namespace，版本更大就换上
    async fn reload(&mut self, namespace: &str) {
        let loaded = self.store.load(namespace).await;
        if let Ok(loaded) = self.check(loaded) {
            self.update(namespace, loaded);
        }
    }

    /// 对一遍所有 namespace 的版本号，只重新读变大的（兜底轮询、重连之后）
    async fn sync_all(&mut self) {
        let revisions = self.store.revisions().await;
        let Ok(revisions) = self.check(revisions) else {
            return;
        };
        for (namespace, revision) in revisions {
            let newer = self
                .states
                .get(&namespace)
                .is_some_and(|state| revision > state.revision);
            if newer {
                self.reload(&namespace).await;
            }
        }
    }

    /// 读到的版本比当前的大才换，然后让用到这个 namespace 的绑定重新拼 struct
    fn update(&mut self, namespace: &str, loaded: Loaded) {
        let current = self.states.get(namespace).map(|state| state.revision);
        if current.is_some_and(|revision| loaded.revision <= revision) {
            return;
        }
        if current.is_some() {
            tracing::info!(
                namespace,
                revision = loaded.revision,
                "live config: namespace changed"
            );
        }
        self.states.insert(
            namespace.to_string(),
            NsState {
                revision: loaded.revision,
                values: loaded.values,
            },
        );
        for binding in &self.bindings {
            if binding.namespaces().iter().any(|name| name == namespace)
                && let Err(e) = binding.apply(&self.states)
            {
                tracing::error!(namespace, "live config: keep the old value, {e:#}");
            }
        }
    }

    /// 查询失败说明连接多半坏了：丢掉，等重连
    fn check<T>(&mut self, result: Result<T>) -> Result<T> {
        if let Err(e) = &result {
            self.failed(e);
        }
        result
    }

    /// 查询、收通知报错：PG 丢掉 listener，主循环下一轮在小池上重建；SQLite 打个日志等下一轮
    fn failed(&mut self, e: &HyErr) {
        if self.store.reset() {
            tracing::warn!("live config: listener failed, rebuilding: {e:#}");
        } else {
            tracing::warn!("live config: query failed, retry on next poll: {e:#}");
        }
    }

    /// 在小池上重建 listener，建好后对一遍版本号补上断线期间的变更。建不起来（库连不上等，小池等过 acquire 超时）
    /// 隔一会儿再试。收到退出信号返回 false
    async fn rebuild_listener(&mut self, shutdown: &CancellationToken) -> bool {
        let opened = tokio::select! {
            _ = shutdown.cancelled() => return false,
            opened = self.store.open(false) => opened,
        };
        match opened {
            Ok(()) => {
                tracing::info!("live config: listener rebuilt");
                self.sync_all().await;
            }
            Err(e) => {
                tracing::warn!("live config: rebuild failed, retry in {RETRY_INTERVAL:?}, {e:#}");
                tokio::select! {
                    _ = shutdown.cancelled() => return false,
                    _ = tokio::time::sleep(RETRY_INTERVAL) => {}
                }
            }
        }
        true
    }
}
