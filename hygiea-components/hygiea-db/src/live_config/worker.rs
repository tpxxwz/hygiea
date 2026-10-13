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

/// 变更历史表留多少行、隔多久清一次
#[derive(Clone, Copy, Debug)]
pub(crate) struct HistoryRetention {
    pub(crate) keep_rows: u64,
    pub(crate) interval: Duration,
}

/// 一个 namespace 最新读到的版本号和表里的值
pub(crate) struct NsState {
    pub(crate) revision: i64,
    pub(crate) values: Map<String, Value>,
    /// 上一次成功换上（绑定都拼成了 struct）时 struct 里这个 namespace 的值，日志里的 diff 和它比；还没生效过是 `None`
    effective: Option<Effective>,
}

/// struct 里一个 namespace 生效的值：`raw` 是普通序列化，比较哪些 key 变了；`shown` 是日志模式
/// （hygiea-core 的 redact）序列化，打日志用：`#[redact(mask)]` 的字段是 `"***"`，`#[redact(skip)]` 的不在里面
#[derive(Clone)]
pub(crate) struct Effective {
    pub(crate) raw: Map<String, Value>,
    pub(crate) shown: Map<String, Value>,
}

/// 绑定到一个 struct 的若干 namespace。任务只看到类型擦除后的这一层
pub(crate) trait Binding: Send {
    fn namespaces(&self) -> &[String];
    /// 按当前各 namespace 的值重新拼出 struct 并替换，返回 struct 里各 namespace 生效的值；转换失败返回错误，旧值不动
    fn apply(&self, states: &HashMap<String, NsState>) -> Result<Vec<(String, Effective)>>;
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
    /// 没配是 `None`，不清
    pub(crate) history: Option<HistoryRetention>,
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
    TrimHistory,
    Signal(Result<Signal>),
}

impl Worker {
    pub(crate) async fn run(mut self, shutdown: CancellationToken) {
        let mut poll = tokio::time::interval(self.poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // interval 第一次立刻触发，启动时刚加载过，跳过
        poll.reset();
        // 启动时就清一次，之后按配置的间隔；没配就不清（间隔随便给一个，不会触发）
        let trim_enabled = self.history.is_some();
        let trim_interval = self
            .history
            .map_or(RETRY_INTERVAL, |history| history.interval);
        let mut trim = tokio::time::interval(trim_interval);
        trim.set_missed_tick_behavior(MissedTickBehavior::Delay);
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
                _ = trim.tick(), if trim_enabled => Event::TrimHistory,
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
                Event::TrimHistory => self.trim_history().await,
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
        // 拼成功、换上了才打：struct 里这个 namespace 现在生效的全部值（表里的值盖在默认值上），打码的字段是 ***
        for (namespace, effective) in binding.apply(&self.states)? {
            if let Some(state) = self.states.get_mut(&namespace) {
                tracing::info!(
                    namespace,
                    revision = state.revision,
                    values = %serde_json::Value::Object(effective.shown.clone()),
                    "live config: namespace loaded"
                );
                state.effective = Some(effective);
            }
        }
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

    /// 变更历史表只留最新的 N 行
    async fn trim_history(&mut self) {
        let Some(history) = self.history else {
            return;
        };
        let trimmed = self.store.trim_history(history.keep_rows).await;
        if let Ok(deleted) = self.check(trimmed)
            && deleted > 0
        {
            tracing::info!(deleted, "live config: history trimmed");
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

    /// 读到的版本比当前的大才换，然后让用到这个 namespace 的绑定重新拼 struct。
    /// 都拼成功了才算生效、打 changed 日志；有一个失败就打 error，diff 的基准还是上一次生效的值
    fn update(&mut self, namespace: &str, loaded: Loaded) {
        let current = self.states.get(namespace);
        if current.is_some_and(|state| loaded.revision <= state.revision) {
            return;
        }
        let effective = current.and_then(|state| state.effective.clone());
        self.states.insert(
            namespace.to_string(),
            NsState {
                revision: loaded.revision,
                values: loaded.values,
                effective: None,
            },
        );
        // 第一次读（bind 里）：还没有绑定用它，bind 拼成功后打 loaded
        let Some(effective) = effective else {
            return;
        };
        let mut applied = None;
        let mut failed = false;
        for binding in &self.bindings {
            if !binding.namespaces().iter().any(|name| name == namespace) {
                continue;
            }
            match binding.apply(&self.states) {
                Ok(effective) => {
                    // 几个绑定用到同一个 namespace 时，日志按第一个绑定的 struct 打
                    if applied.is_none() {
                        applied = effective.into_iter().find(|(name, _)| name == namespace);
                    }
                }
                Err(e) => {
                    failed = true;
                    tracing::error!(namespace, "live config: keep the old value, {e:#}");
                }
            }
        }
        let Some(state) = self.states.get_mut(namespace) else {
            return;
        };
        match applied {
            Some((_, now)) if !failed => {
                let changes = diff(&effective, &now, &state.values);
                if !changes.is_empty() {
                    tracing::info!(
                        namespace,
                        revision = state.revision,
                        changes = %changes.join("; "),
                        "live config: namespace changed"
                    );
                }
                state.effective = Some(now);
            }
            _ => state.effective = Some(effective),
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

/// 生效的值前后的差别，struct 里值变了的 key 每个一项：`key: 旧值 -> 新值`，打码的字段是 `***`；
/// `#[redact(skip)]` 的字段只写 `key: (hidden)`。表里删掉了这一行（回到代码里的默认值）的后面加 `(default)`。
/// 表里多出来、struct 里没有的 key 不生效，不在这里（merge 时打 warn）
fn diff(old: &Effective, new: &Effective, table: &Map<String, Value>) -> Vec<String> {
    new.raw
        .iter()
        .filter(|(key, value)| old.raw.get(*key) != Some(*value))
        .map(|(key, _)| {
            let line = match (old.shown.get(key), new.shown.get(key)) {
                (Some(before), Some(after)) => format!("{key}: {before} -> {after}"),
                _ => format!("{key}: (hidden)"),
            };
            if table.contains_key(key) {
                line
            } else {
                format!("{line} (default)")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    fn effective(raw: Value, shown: Value) -> Effective {
        Effective {
            raw: object(raw),
            shown: object(shown),
        }
    }

    /// 只列值变了的 key；打码的显示 ***，skip 的只说变了；表里没有这行的标 (default)
    #[test]
    fn diff_lists_effective_changes() {
        use serde_json::json;
        let old = effective(
            json!({ "a": 1, "b": "x", "token": "t1", "id": 7, "c": true }),
            json!({ "a": 1, "b": "x", "token": "***", "c": true }),
        );
        let new = effective(
            json!({ "a": 2, "b": "x", "token": "t2", "id": 8, "c": false }),
            json!({ "a": 2, "b": "x", "token": "***", "c": false }),
        );
        let table = object(json!({ "a": 2, "b": "x", "token": "t2", "id": 8 }));
        // serde_json 开了 preserve_order 时 key 按 struct 字段顺序，这里排一下再比
        let mut changes = diff(&old, &new, &table);
        changes.sort();
        assert_eq!(
            changes,
            [
                "a: 1 -> 2",
                "c: true -> false (default)",
                "id: (hidden)",
                "token: \"***\" -> \"***\""
            ]
        );
    }
}
