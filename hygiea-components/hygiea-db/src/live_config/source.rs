//! 一个连接池的动态配置：保管 [`Store`]，第一次绑定时连库、起后台任务；`bind` / `bind_all` 把默认值转成 JSON、
//! 建出绑定交给后台任务。绑定（[`TypedBinding`]）负责把默认值和表里的值合并、转成用户的 struct、发出去

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::{OnceCell, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use hygiea_core::app::{AppErr, CancellationToken};
use hygiea_core::{BaseErr, Result, ResultExt, ensure, err};

use super::Backend;
use super::LiveConfigConfig;
use super::Store;
use super::handle::LiveConfig;
use super::worker::{Binding, Command, Effective, HistoryRetention, NsState, Worker};
use crate::RepoErr;

/// 一个动态配置源：后台任务、小池都在这里。[`DbLiveConfig`](super::DbLiveConfig) 持有它。
/// `Clone` 后共用同一个后台任务
#[derive(Clone)]
pub(crate) struct LiveConfigSource {
    inner: Arc<Inner>,
}

struct Inner {
    backend: Backend,
    /// 独立小池和在上面做的事。第一次 bind 时交给后台任务，之后这里是 `None`
    store: std::sync::Mutex<Option<Store>>,
    /// 配置表名，Debug 用
    table: String,
    poll_interval: Duration,
    /// 历史表留多少行、隔多久清一次，没配是 `None`
    history: Option<HistoryRetention>,
    /// 第一次 bind 时连库、起后台任务，之后发命令给它
    commands: OnceCell<mpsc::Sender<Command>>,
    handle: std::sync::Mutex<Option<JoinHandle<()>>>,
    shutdown: CancellationToken,
}

impl std::fmt::Debug for LiveConfigSource {
    // 连接参数里有密码，不打
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveConfigSource")
            .field("backend", &self.inner.backend)
            .field("table", &self.inner.table)
            .finish_non_exhaustive()
    }
}

impl LiveConfigSource {
    /// 只校验配置，不连库（小池第一次用时才连）
    pub(super) fn new<C>(store: Store, config: &LiveConfigConfig<C>) -> Result<Self> {
        // 有推送（PG 的 LISTEN）的轮询只是兜底；没有的（SQLite）主要靠它
        let backend = store.backend();
        let poll_interval_secs =
            config
                .poll_interval_secs
                .unwrap_or(if store.has_push() { 60 } else { 1 });
        ensure!(poll_interval_secs > 0, BaseErr::NotPositive, {
            "param": "live_config.poll_interval_secs",
            "op": "LiveConfigSource::new",
        });
        let history = history_retention(config)?;
        Ok(Self {
            inner: Arc::new(Inner {
                backend,
                table: store.table(),
                store: std::sync::Mutex::new(Some(store)),
                poll_interval: Duration::from_secs(poll_interval_secs),
                history,
                commands: OnceCell::new(),
                handle: std::sync::Mutex::new(None),
                shutdown: CancellationToken::new(),
            }),
        })
    }

    /// 组件关闭时调用：后台任务退出、小池关掉，之后 bind 报 [`RepoErr::LiveConfigClosed`]
    pub(crate) async fn close(&self) {
        self.inner.shutdown.cancel();
        let handle = self
            .inner
            .handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        // 后台任务退出时自己关小池；没起过任务（没 bind 过）的在这里关
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        let store = self.take_store();
        if let Some(mut store) = store {
            store.close().await;
        }
    }

    /// 把一个 namespace 绑定到 `T`：`T` 的第一层字段是 key。`defaults` 是表里没有的 key 用的值
    pub(crate) async fn bind<T>(&self, namespace: &str, defaults: T) -> Result<LiveConfig<T>>
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static,
    {
        let values = to_object(&defaults, namespace)?;
        let namespaces = vec![namespace.to_string()];
        self.register(
            Shape::One,
            namespaces,
            vec![(namespace.to_string(), values)],
            defaults,
        )
        .await
    }

    /// 把多个 namespace 绑定到 `T`：`T` 的第一层字段是 namespace，第二层是 key
    pub(crate) async fn bind_all<T>(&self, defaults: T) -> Result<LiveConfig<T>>
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static,
    {
        let root = to_object(&defaults, "*")?;
        let mut namespaces = Vec::with_capacity(root.len());
        let mut values = Vec::with_capacity(root.len());
        for (namespace, value) in root {
            let Value::Object(object) = value else {
                return Err(err!(RepoErr::LiveConfigDefaultsInvalid, namespace));
            };
            namespaces.push(namespace.clone());
            values.push((namespace, object));
        }
        self.register(Shape::All, namespaces, values, defaults)
            .await
    }

    fn take_store(&self) -> Option<Store> {
        self.inner
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// 后台任务的命令通道，第一次调用时连库、检查（缺了就建）表、起任务
    async fn commands(&self) -> Result<&mpsc::Sender<Command>> {
        self.inner
            .commands
            .get_or_try_init(|| async {
                let inner = &self.inner;
                if inner.shutdown.is_cancelled() {
                    return Err(err!(RepoErr::LiveConfigClosed));
                }
                let Some(mut store) = self.take_store() else {
                    return Err(err!(RepoErr::LiveConfigClosed));
                };
                if let Err(e) = store.open(true).await {
                    // 没连上（库连不上、表对不上等）：放回去，下次 bind 再试
                    *inner
                        .store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(store);
                    return Err(e);
                }
                let (commands, receiver) = mpsc::channel(16);
                let worker = Worker {
                    poll_interval: inner.poll_interval,
                    history: inner.history,
                    store,
                    commands: receiver,
                    states: HashMap::new(),
                    bindings: Vec::new(),
                };
                let handle = tokio::spawn(worker.run(inner.shutdown.clone()));
                *inner
                    .handle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
                Ok(commands)
            })
            .await
    }

    async fn register<T>(
        &self,
        shape: Shape,
        namespaces: Vec<String>,
        defaults: Vec<(String, Map<String, Value>)>,
        initial: T,
    ) -> Result<LiveConfig<T>>
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static,
    {
        let commands = self.commands().await?;
        let (sender, mut receiver) = watch::channel(Arc::new(initial));
        let binding = TypedBinding {
            shape,
            namespaces,
            defaults: defaults.clone(),
            sender,
        };
        let (reply, replied) = oneshot::channel();
        commands
            .send(Command::Bind {
                defaults,
                binding: Box::new(binding),
                reply,
            })
            .await
            .map_err(|_| err!(RepoErr::LiveConfigClosed))?;
        replied
            .await
            .map_err(|_| err!(RepoErr::LiveConfigClosed))??;
        // 首次加载的值算已经看过，changed() 只等之后的变化
        receiver.borrow_and_update();
        Ok(LiveConfig::new(receiver))
    }
}

/// `history_keep_rows`、`history_trim_interval_mins` 要么都配、要么都不配，配了都要是正数
fn history_retention<C>(config: &LiveConfigConfig<C>) -> Result<Option<HistoryRetention>> {
    let (keep_rows, interval_mins) = match (
        config.history_keep_rows,
        config.history_trim_interval_mins,
    ) {
        (None, None) => return Ok(None),
        (Some(rows), Some(mins)) => (rows, mins),
        _ => {
            return Err(err!(
                AppErr::InvalidConfig,
                "live_config.history_keep_rows and history_trim_interval_mins must be set together"
            ));
        }
    };
    ensure!(keep_rows > 0, BaseErr::NotPositive, {
        "param": "live_config.history_keep_rows",
        "op": "LiveConfigSource::new",
    });
    ensure!(interval_mins > 0, BaseErr::NotPositive, {
        "param": "live_config.history_trim_interval_mins",
        "op": "LiveConfigSource::new",
    });
    Ok(Some(HistoryRetention {
        keep_rows,
        interval: Duration::from_secs(interval_mins.saturating_mul(60)),
    }))
}

/// 默认值转成 JSON 对象
fn to_object<T: Serialize>(value: &T, namespace: &str) -> Result<Map<String, Value>> {
    match serde_json::to_value(value) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(err!(RepoErr::LiveConfigDefaultsInvalid, namespace)),
        Err(e) => Err(e).wrap_err(|| err!(RepoErr::LiveConfigDefaultsInvalid, namespace)),
    }
}

// ---- 绑定 ---------------------------------------------------------------------

/// `bind` 是一个 namespace 的 key 直接对应字段；`bind_all` 是第一层 namespace、第二层 key
enum Shape {
    One,
    All,
}

struct TypedBinding<T> {
    shape: Shape,
    namespaces: Vec<String>,
    defaults: Vec<(String, Map<String, Value>)>,
    sender: watch::Sender<Arc<T>>,
}

impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> Binding for TypedBinding<T> {
    fn namespaces(&self) -> &[String] {
        &self.namespaces
    }

    fn apply(&self, states: &HashMap<String, NsState>) -> Result<Vec<(String, Effective)>> {
        let mut merged = self.defaults.iter().map(|(namespace, defaults)| {
            (
                namespace.clone(),
                merge(namespace, defaults, states.get(namespace)),
            )
        });
        let (json, name) = match self.shape {
            Shape::One => match merged.next() {
                Some((namespace, object)) => (Value::Object(object), namespace),
                None => return Ok(Vec::new()),
            },
            Shape::All => {
                let root = merged
                    .map(|(namespace, object)| (namespace, Value::Object(object)))
                    .collect();
                (Value::Object(root), self.namespaces.join(","))
            }
        };
        let value: T = serde_json::from_value(json)
            .wrap_err(|| err!(RepoErr::LiveConfigDecodeFailed, name))?;
        let effective = self.effective(&value);
        self.sender.send_replace(Arc::new(value));
        Ok(effective)
    }
}

impl<T: Serialize> TypedBinding<T> {
    /// struct 里各 namespace 生效的值，给日志用：普通序列化一份比较变化，日志模式（redact）序列化一份打出来。
    /// 刚从 JSON 转出来的 struct 再序列化不会失败，万一失败就当成空的，只影响日志
    fn effective(&self, value: &T) -> Vec<(String, Effective)> {
        let raw = serde_json::to_value(value).unwrap_or_default();
        let shown = hygiea_core::redact::scope(|| serde_json::to_value(value)).unwrap_or_default();
        let part = |root: &Value, namespace: &str| match self.shape {
            Shape::One => object(root),
            Shape::All => root.get(namespace).map(object).unwrap_or_default(),
        };
        self.namespaces
            .iter()
            .map(|namespace| {
                let effective = Effective {
                    raw: part(&raw, namespace),
                    shown: part(&shown, namespace),
                };
                (namespace.clone(), effective)
            })
            .collect()
    }
}

/// JSON 对象里的字段，不是对象时是空的
fn object(value: &Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    }
}

/// 表里的值覆盖到默认值上。只认默认值里有的 key，多出来的打 warn
fn merge(
    namespace: &str,
    defaults: &Map<String, Value>,
    state: Option<&NsState>,
) -> Map<String, Value> {
    let mut object = defaults.clone();
    if let Some(state) = state {
        for (key, value) in &state.values {
            if object.contains_key(key) {
                object.insert(key.clone(), value.clone());
            } else {
                tracing::warn!(namespace, key, "live config: key not in struct, ignored");
            }
        }
    }
    object
}
