//! 对外的句柄：[`LiveConfig`]（绑定拿到的整份配置）、[`LiveView`]（其中一块）、[`LiveRef`]（`LiveView::get` 的快照）、
//! [`LiveConfigFields`]（`#[derive(hy_live_config)]` 生成按字段读的句柄）、[`OnChangeGuard`]（`on_change` 回调的后台任务）

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::watch;

use hygiea_core::{Result, err};

use crate::RepoErr;

/// 绑定到 struct 的配置，`Clone` 后各处共用同一份（`T` 不用实现 `Clone`）
pub struct LiveConfig<T> {
    pub(super) receiver: watch::Receiver<Arc<T>>,
    /// 这个句柄上一次观察到的那一版：`changed_by` 拿它当基准，见[模块文档](super)
    seen: Arc<T>,
    /// [`fixed`](Self::fixed) 建的才有：留着发送端，`changed()` 一直等不到，而不是马上报后台任务已退出
    fixed: Option<Arc<watch::Sender<Arc<T>>>>,
}

impl<T> Clone for LiveConfig<T> {
    fn clone(&self) -> Self {
        Self {
            receiver: self.receiver.clone(),
            seen: self.seen.clone(),
            fixed: self.fixed.clone(),
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for LiveConfig<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LiveConfig").field(&*self.get()).finish()
    }
}

impl<T> LiveConfig<T> {
    /// 连接池 bind 出来的句柄。当前的值算已经观察过
    pub(super) fn new(mut receiver: watch::Receiver<Arc<T>>) -> Self {
        let seen = receiver.borrow_and_update().clone();
        Self {
            receiver,
            seen,
            fixed: None,
        }
    }

    /// 固定值、不连库的句柄：业务的单元测试里用它代替 `DbLiveConfig` 的 `bind` / `bind_all` 绑出来的，
    /// `get()`、`view()` 照常用，值不会变，`changed()` 一直等不到
    ///
    /// ```ignore
    /// let runtime = LiveConfig::fixed(RuntimeConfig { markji_sync: MarkjiSync { auto_sync: true, ..Default::default() }, ..Default::default() });
    /// let service = MarkjiService::new(runtime.view(|c| &c.markji_sync));
    /// ```
    pub fn fixed(value: T) -> Self {
        let (sender, receiver) = watch::channel(Arc::new(value));
        let mut config = Self::new(receiver);
        config.fixed = Some(Arc::new(sender));
        config
    }

    /// 当前最新的值。内部是 tokio `watch` 的读写锁：读锁里只 clone 一下 `Arc`（引用计数 +1）就放开，
    /// 拿到的是这一刻的快照，之后用它不持有锁，也不会被后来的新值改掉。一次处理里要用几个配置项时只 `get()` 一次，
    /// 这几个值就来自同一个版本
    pub fn get(&self) -> Arc<T> {
        self.receiver.borrow().clone()
    }

    /// 借当前最新的那一版给 `f` 读一下，返回 `f` 的结果，比如 `config.read(|c| c.sync.enabled)`。
    /// 和 `get()` 一样只 clone 一下 `Arc`，`f` 执行时不持有读锁；不更新观察过的版本
    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.get())
    }

    /// 等到值变了再返回：这个句柄上一次观察之后有新版本就马上返回（见[模块文档](super)），返回时观察到的是当时最新的那一版。
    /// 后台任务退出了返回 [`RepoErr::LiveConfigClosed`]
    pub async fn changed(&mut self) -> Result<()> {
        self.receiver
            .changed()
            .await
            .map_err(|_| err!(RepoErr::LiveConfigClosed))?;
        self.seen = self.receiver.borrow_and_update().clone();
        Ok(())
    }

    /// 等到 `project` 取出的那个字段变了再返回，其他字段变了接着等：
    ///
    /// ```ignore
    /// runtime.changed_by(|c| &c.markji_sync.start_time).await?;
    /// ```
    ///
    /// 基准是这个句柄上一次观察到的那一版（见[模块文档](super)）：先拿当前最新的值和它比，已经不一样就马上返回，
    /// 所以调用之前已经发生的变化不会漏。按值比较（`PartialEq`），中间改过又改回来、和基准一样的不算变化。
    /// 返回时观察到的是那个字段变了的那一版；后台任务退出了返回 [`RepoErr::LiveConfigClosed`]
    pub async fn changed_by<V: PartialEq>(&mut self, project: fn(&T) -> &V) -> Result<()> {
        self.changed_when(|seen, current| project(seen) != project(current))
            .await
    }

    /// `changed_by` 的实现：当前的值和观察过的那一版 `differs` 就返回，否则等下一个版本再比。
    /// 每比一次都把观察过的版本推进到比过的那一版
    async fn changed_when(&mut self, mut differs: impl FnMut(&T, &T) -> bool) -> Result<()> {
        loop {
            let current = self.receiver.borrow_and_update().clone();
            let hit = differs(&self.seen, &current);
            self.seen = current;
            if hit {
                return Ok(());
            }
            self.receiver
                .changed()
                .await
                .map_err(|_| err!(RepoErr::LiveConfigClosed))?;
        }
    }
}

impl<T: Send + Sync + 'static> LiveConfig<T> {
    /// 从整份配置里指向其中一块（比如 `bind_all` 的一个 namespace），交给只用这一块的地方：
    ///
    /// ```ignore
    /// let runtime = source.bind_all(RuntimeConfig::default()).await?;
    /// let markji = MarkjiService::new(runtime.view(|c| &c.markji_sync));   // LiveView<MarkjiSync>
    /// // MarkjiService 里：self.config.get().auto_sync
    /// ```
    ///
    /// 不重新绑定、不复制配置，`get()` 每次从整份配置当前的快照里取这一块
    pub fn view<U: 'static>(&self, project: fn(&T) -> &U) -> LiveView<U> {
        LiveView {
            source: Box::new(Projection {
                config: self.clone(),
                project,
            }),
        }
    }

    /// `#[derive(hy_live_config)]` 生成的句柄（`T` 是 `Foo` 就是 `FooLiveConfig`），按字段读，每次读到最新的值，见 [`LiveConfigFields`]
    pub fn fields(&self) -> T::Handle
    where
        T: LiveConfigFields,
    {
        T::handle(self.view(|c| c))
    }

    /// 整份配置变了就调一次 `f`，见 [`LiveView::on_change`]
    pub fn on_change(&self, f: impl Fn(&T) + Send + Sync + 'static) -> OnChangeGuard {
        self.view(|c| c).on_change(f)
    }

    /// `project` 取出的那个字段变了才调 `f`，见 [`LiveView::on_change_by`]
    pub fn on_change_by<V: PartialEq + 'static>(
        &self,
        project: fn(&T) -> &V,
        f: impl Fn(&V) + Send + Sync + 'static,
    ) -> OnChangeGuard {
        self.view(|c| c).on_change_by(project, f)
    }
}

/// 整份配置里的一块，[`LiveConfig::view`] 拿到，`Clone` 后各处共用。字段类型里不带整份配置的类型
pub struct LiveView<U> {
    source: Box<dyn Source<U>>,
}

impl<U> Clone for LiveView<U> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone_box(),
        }
    }
}

impl<U: std::fmt::Debug> std::fmt::Debug for LiveView<U> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LiveView").field(&*self.get()).finish()
    }
}

impl<U> LiveView<U> {
    /// 这一块当前最新的值，和 [`LiveConfig::get`] 一样是快照：拿着它期间整份配置换了新值也不影响手上这份。
    /// 像 `&U` 一样直接点字段
    pub fn get(&self) -> LiveRef<U> {
        self.source.get()
    }

    /// 借这一块当前最新的值给 `f` 读一下，返回 `f` 的结果，比如 `view.read(|c| c.enabled)`。
    /// 和 `get()` 拿到的是同样的值，但不分配 [`LiveRef`]：只 clone 一下整份配置的 `Arc`，`f` 执行时不持有读锁。
    /// 不更新观察过的版本。
    ///
    /// `f` 是 `FnMut` 而不是 `FnOnce`：内部经过抹掉类型的一层，编译器看不出 `f` 一定被调用过，
    /// 万一没调到时退回 `get()` 再调一次，不用 `unwrap`
    pub fn read<R>(&self, mut f: impl FnMut(&U) -> R) -> R {
        let mut out = None;
        self.source.read(&mut |value| out = Some(f(value)));
        match out {
            Some(out) => out,
            None => f(&self.get()),
        }
    }

    /// 等到整份配置变了再返回（不只是这一块变了），基准和 [`LiveConfig::changed`] 一样。
    /// 后台任务退出了返回 [`RepoErr::LiveConfigClosed`]
    pub async fn changed(&mut self) -> Result<()> {
        self.source.changed().await
    }

    /// 等到 `project` 取出的那个字段变了再返回，基准和比较方式见 [`LiveConfig::changed_by`]
    pub async fn changed_by<V: PartialEq>(&mut self, project: fn(&U) -> &V) -> Result<()> {
        self.source
            .changed_when(Box::new(move |seen, current| {
                project(seen) != project(current)
            }))
            .await
    }
}

impl<U: 'static> LiveView<U> {
    /// 从这一块里再往下指向其中一块，可以一层层接着截：
    ///
    /// ```ignore
    /// let markji = runtime.view(|c| &c.markji_sync);     // LiveView<MarkjiSync>
    /// let retry = markji.view(|c| &c.retry);             // LiveView<Retry>
    /// ```
    ///
    /// 和 [`LiveConfig::view`] 一样不复制配置，每次从整份配置当前的快照里取；观察过的版本从这个句柄接着算
    pub fn view<V: 'static>(&self, project: fn(&U) -> &V) -> LiveView<V> {
        LiveView {
            source: Box::new(Chain {
                inner: self.source.clone_box(),
                project,
            }),
        }
    }

    /// `#[derive(hy_live_config)]` 生成的句柄，见 [`LiveConfig::fields`]
    pub fn fields(&self) -> U::Handle
    where
        U: LiveConfigFields,
    {
        U::handle(self.clone())
    }

    /// 整份配置变了就调一次 `f`，参数是这一块变了之后的值（可能这一块本身没变，只是别的地方变了）。
    /// 只管这一块里某个字段的变化用 [`on_change_by`](Self::on_change_by)。
    ///
    /// ```ignore
    /// let guard = markji.on_change(|c| tracing::info!(?c, "markji_sync changed"));   // 存着 guard，drop 了就不再回调
    /// ```
    ///
    /// - 内部 `tokio::spawn` 一个任务循环 `changed()`，要在 tokio 运行时里调用（和 `tokio::spawn` 一样，不在运行时里会 panic）。
    ///   基准是这个句柄当时观察到的那一版：它之后还没观察过的变化会马上回调一次，之后每有新版本回调一次。
    ///   一下子来了几个版本时可能合成一次，回调拿到的是最新的那一版
    /// - 返回的 [`OnChangeGuard`] drop 掉任务就停；配置组件关闭后（`changed()` 返回 [`RepoErr::LiveConfigClosed`]）任务自己退出
    /// - `f` 在后台任务里同步执行，执行期间不会收到下一次回调，别在里面做耗时的事，要等 IO 的自己 `spawn` 出去
    /// - `f` panic 了：捕获下来打 error 日志，任务接着监听，下次变化照常回调。编译选项是 `panic = "abort"` 时捕获不了，进程直接退出
    pub fn on_change(&self, f: impl Fn(&U) + Send + Sync + 'static) -> OnChangeGuard {
        let mut view = self.clone();
        OnChangeGuard::spawn(async move {
            while view.changed().await.is_ok() {
                view.source
                    .read_seen(&mut |value| call_guarded(|| f(value)));
            }
        })
    }

    /// `project` 取出的那个字段变了才调 `f`，参数是这个字段的新值。是否变了按 [`changed_by`](Self::changed_by) 判断
    /// （和观察过的那一版按值比较），其余和 [`on_change`](Self::on_change) 一样。
    ///
    /// ```ignore
    /// let guard = markji.on_change_by(|c| &c.start_time, |t| tracing::info!(%t, "start_time changed"));
    /// ```
    pub fn on_change_by<V: PartialEq + 'static>(
        &self,
        project: fn(&U) -> &V,
        f: impl Fn(&V) + Send + Sync + 'static,
    ) -> OnChangeGuard {
        let mut view = self.clone();
        OnChangeGuard::spawn(async move {
            while view.changed_by(project).await.is_ok() {
                view.source
                    .read_seen(&mut |value| call_guarded(|| f(project(value))));
            }
        })
    }
}

/// [`LiveView::get`] 拿到的快照，`Deref` 到 `U`。里面拿着整份配置那一版的 `Arc`
pub struct LiveRef<U> {
    held: Box<dyn Held<U>>,
}

impl<U> std::ops::Deref for LiveRef<U> {
    type Target = U;

    fn deref(&self) -> &U {
        self.held.value()
    }
}

impl<U: 'static> LiveRef<U> {
    /// 从快照里取其中一个字段，还是同一版的快照（不复制字段的值）：
    ///
    /// ```ignore
    /// let levels: LiveRef<Vec<i32>> = markji.get().map(|c| &c.levels);
    /// ```
    pub fn map<V: 'static>(self, project: fn(&U) -> &V) -> LiveRef<V> {
        LiveRef {
            held: Box::new(Mapped {
                inner: self.held,
                project,
            }),
        }
    }
}

/// `#[derive(hy_live_config)]` 实现它：`Handle` 是生成的按字段读的句柄（`Foo` 生成 `FooLiveConfig`）。
/// 从 [`LiveConfig::fields`]、[`LiveView::fields`] 拿到，不用直接调 [`handle`](Self::handle)。
///
/// ```ignore
/// use hygiea::db::live_config::hy_live_config;
///
/// #[derive(Serialize, Deserialize, Default, hy_live_config)]
/// pub struct RuntimeConfig {
///     #[live_config(nested)]                    // 字段方法返回 MarkjiSyncLiveConfig，可以接着按字段读
///     pub markji_sync: MarkjiSync,
/// }
///
/// #[derive(Serialize, Deserialize, Default, hy_live_config)]
/// pub struct MarkjiSync {
///     pub auto_sync: bool,               // 默认：返回字段值的 clone
///     #[live_config(by_ref)]                    // 不复制：返回 LiveRef<Vec<i32>>
///     pub levels: Vec<i32>,
///     #[live_config(nested)]
///     pub retry: Retry,
/// }
///
/// #[derive(Serialize, Deserialize, Default, hy_live_config)]
/// pub struct Retry {
///     pub times: u32,
/// }
///
/// let runtime = source.bind_all(RuntimeConfig::default()).await?;
/// let markji = runtime.fields().markji_sync();   // MarkjiSyncLiveConfig，交给 service 存着
/// // service 里：每次调都读到最新的值
/// if markji.auto_sync() { .. }
/// let times = markji.retry().times();
/// // 几个字段要来自同一版时用整块快照
/// let snapshot = markji.snapshot();            // LiveRef<MarkjiSync>
/// ```
///
/// 生成的句柄（`Clone`，可见性和 struct 相同）的方法：
/// - 每个具名字段一个同名方法，可见性和字段相同：默认返回字段值的 clone（要求字段实现 `Clone`）；
///   `#[live_config(by_ref)]` 返回 [`LiveRef`]，不复制；`#[live_config(nested)]` 返回字段类型的句柄（字段类型也要 `#[derive(hy_live_config)]`）
/// - `snapshot()`：整块的快照 [`LiveRef`]
/// - `changed()`、`changed_by()`、`on_change()`、`on_change_by()`：同 [`LiveView`] 的同名方法
///
/// 字段方法每次调用都读当时最新的那一版，连续调几个字段方法，中间配置可能换了版本，拿到的值可能来自不同版本；
/// 要几个值来自同一版用 `snapshot()`。
///
/// 宏只支持具名字段、不带泛型的 struct；字段名不能和上面这几个方法（以及 `clone`）重名。
/// 生成的代码默认引用 `::hygiea::db::live_config`，hygiea 改过名或者经别的 crate 转导出时用
/// `#[hy_live_config(crate = "path")]` 写 facade 的路径（后面接 `::db::live_config`）
pub trait LiveConfigFields: Sized + 'static {
    /// 生成的句柄类型
    type Handle;

    /// 用这一块的 [`LiveView`] 建句柄
    fn handle(view: LiveView<Self>) -> Self::Handle;
}

/// [`LiveView::on_change`] / [`LiveView::on_change_by`] 返回：持有回调的后台任务，drop 掉任务就停。
/// 要一直监听就把它存着（比如放进 AppState），写成 `let _ = ..` 会马上 drop
#[must_use = "drop 掉 guard 回调的任务就停了"]
#[derive(Debug)]
pub struct OnChangeGuard {
    task: tokio::task::JoinHandle<()>,
}

impl OnChangeGuard {
    fn spawn(task: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            task: tokio::spawn(async move {
                task.await;
                tracing::debug!("live config: closed, on_change task stopped");
            }),
        }
    }
}

impl Drop for OnChangeGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 执行回调，panic 了捕获下来打日志，不让后台任务跟着退出
fn call_guarded(f: impl FnOnce()) {
    if let Err(panic) = std::panic::catch_unwind(AssertUnwindSafe(f)) {
        let message = panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic payload>");
        tracing::error!("live config: on_change callback panicked, keep listening: {message}");
    }
}

type Changed<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// `changed_by` 往下传的比较：`(观察过的那一版, 当前的)` 不一样时返回 `true`
type Differs<'a, U> = Box<dyn FnMut(&U, &U) -> bool + Send + 'a>;

/// 抹掉整份配置的类型 `T`：[`LiveView`] 只带 `U`
trait Source<U>: Send + Sync {
    fn get(&self) -> LiveRef<U>;
    /// 当前最新的那一版借给 `f`，不分配
    fn read(&self, f: &mut dyn FnMut(&U));
    /// 句柄观察过的那一版借给 `f`：`on_change` 回调用，和 `changed` 返回时比的是同一版
    fn read_seen(&self, f: &mut dyn FnMut(&U));
    fn changed(&mut self) -> Changed<'_>;
    fn changed_when<'a>(&'a mut self, differs: Differs<'a, U>) -> Changed<'a>;
    fn clone_box(&self) -> Box<dyn Source<U>>;
}

/// 整份配置里的一块：[`LiveConfig::view`]
struct Projection<T, U> {
    config: LiveConfig<T>,
    project: fn(&T) -> &U,
}

impl<T: Send + Sync + 'static, U: 'static> Source<U> for Projection<T, U> {
    fn get(&self) -> LiveRef<U> {
        LiveRef {
            held: Box::new(Snapshot {
                config: self.config.get(),
                project: self.project,
            }),
        }
    }

    fn read(&self, f: &mut dyn FnMut(&U)) {
        let current = self.config.get();
        f((self.project)(&current));
    }

    fn read_seen(&self, f: &mut dyn FnMut(&U)) {
        f((self.project)(&self.config.seen));
    }

    fn changed(&mut self) -> Changed<'_> {
        Box::pin(self.config.changed())
    }

    fn changed_when<'a>(&'a mut self, mut differs: Differs<'a, U>) -> Changed<'a> {
        let project = self.project;
        Box::pin(
            self.config.changed_when(move |seen: &T, current: &T| {
                differs(project(seen), project(current))
            }),
        )
    }

    fn clone_box(&self) -> Box<dyn Source<U>> {
        Box::new(Projection {
            config: self.config.clone(),
            project: self.project,
        })
    }
}

/// 一块里再往下的一块：[`LiveView::view`]
struct Chain<U, V> {
    inner: Box<dyn Source<U>>,
    project: fn(&U) -> &V,
}

impl<U: 'static, V: 'static> Source<V> for Chain<U, V> {
    fn get(&self) -> LiveRef<V> {
        self.inner.get().map(self.project)
    }

    fn read(&self, f: &mut dyn FnMut(&V)) {
        let project = self.project;
        self.inner.read(&mut |value| f(project(value)));
    }

    fn read_seen(&self, f: &mut dyn FnMut(&V)) {
        let project = self.project;
        self.inner.read_seen(&mut |value| f(project(value)));
    }

    fn changed(&mut self) -> Changed<'_> {
        self.inner.changed()
    }

    fn changed_when<'a>(&'a mut self, mut differs: Differs<'a, V>) -> Changed<'a> {
        let project = self.project;
        self.inner
            .changed_when(Box::new(move |seen: &U, current: &U| {
                differs(project(seen), project(current))
            }))
    }

    fn clone_box(&self) -> Box<dyn Source<V>> {
        Box::new(Chain {
            inner: self.inner.clone_box(),
            project: self.project,
        })
    }
}

trait Held<U>: Send + Sync {
    fn value(&self) -> &U;
}

/// 整份配置的一版，加上从里面取哪一块
struct Snapshot<T, U> {
    config: Arc<T>,
    project: fn(&T) -> &U,
}

impl<T: Send + Sync, U> Held<U> for Snapshot<T, U> {
    fn value(&self) -> &U {
        (self.project)(&self.config)
    }
}

/// 快照里再取一个字段：[`LiveRef::map`]
struct Mapped<U, V> {
    inner: Box<dyn Held<U>>,
    project: fn(&U) -> &V,
}

impl<U, V> Held<V> for Mapped<U, V> {
    fn value(&self) -> &V {
        (self.project)(self.inner.value())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;

    #[derive(Debug, PartialEq)]
    struct Config {
        sync: Sync,
    }

    #[derive(Debug, PartialEq)]
    struct Sync {
        enabled: bool,
    }

    /// 三层：整份配置 → 一块 → 再一块
    #[derive(Clone, Debug, Default, PartialEq)]
    struct Root {
        name: String,
        job: Job,
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    struct Job {
        enabled: bool,
        levels: Vec<i32>,
        retry: Retry,
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    struct Retry {
        times: u32,
    }

    /// 等不到时等多久算"不会返回"
    const WAIT: Duration = Duration::from_millis(50);

    /// 后台任务那一端：发新值、drop 掉模拟组件关闭
    fn channel<T>(initial: T) -> (watch::Sender<Arc<T>>, LiveConfig<T>) {
        let (sender, receiver) = watch::channel(Arc::new(initial));
        (sender, LiveConfig::new(receiver))
    }

    fn update<T: Clone>(sender: &watch::Sender<Arc<T>>, f: impl FnOnce(&mut T)) {
        let mut next = (**sender.borrow()).clone();
        f(&mut next);
        sender.send_replace(Arc::new(next));
    }

    /// 等着 `cond` 成立，后台任务里的回调要等一下才跑到
    async fn eventually(cond: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !cond() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("条件一直不成立");
    }

    /// 固定值：get / view 拿到给的值，changed 一直等不到（不会马上报已关闭）
    #[tokio::test]
    async fn fixed_never_changes() {
        let mut config = LiveConfig::fixed(Config {
            sync: Sync { enabled: true },
        });
        assert!(config.get().sync.enabled);
        let mut view = config.view(|c| &c.sync);
        assert_eq!(*view.get(), Sync { enabled: true });

        let waited = tokio::time::timeout(WAIT, config.changed()).await;
        assert!(waited.is_err(), "changed() 不应该返回");
        let waited = tokio::time::timeout(WAIT, view.changed()).await;
        assert!(waited.is_err());
        let waited = tokio::time::timeout(WAIT, view.changed_by(|c| &c.enabled)).await;
        assert!(waited.is_err());
    }

    /// view 接着 view：每层 get / read 都读到整份配置最新的值，get 拿到的快照不跟着变
    #[test]
    fn view_chain_reads_latest() {
        let (sender, config) = channel(Root::default());
        let job = config.view(|c| &c.job);
        let retry = job.view(|c| &c.retry);
        let snapshot = retry.get();

        update(&sender, |c| c.job.retry.times = 3);
        assert_eq!(retry.get().times, 3);
        assert_eq!(retry.read(|c| c.times), 3);
        assert_eq!(job.read(|c| c.retry.times), 3);
        assert_eq!(config.read(|c| c.job.retry.times), 3);
        assert_eq!(snapshot.times, 0, "拿到的快照不变");
        assert_eq!(format!("{retry:?}"), "LiveView(Retry { times: 3 })");
    }

    /// LiveRef::map：快照里取一个字段，还是那一版
    #[test]
    fn live_ref_map_keeps_snapshot() {
        let (sender, config) = channel(Root::default());
        let job = config.view(|c| &c.job);
        let levels = job.get().map(|c| &c.levels);
        let times = job.get().map(|c| &c.retry).map(|r| &r.times);

        update(&sender, |c| {
            c.job.levels = vec![1];
            c.job.retry.times = 5;
        });
        assert!(levels.is_empty());
        assert_eq!(*times, 0);
        assert_eq!(*job.get().map(|c| &c.levels), vec![1]);
    }

    /// changed_by：别的字段变了接着等，盯的字段变了才返回
    #[tokio::test]
    async fn changed_by_ignores_other_fields() {
        let (sender, config) = channel(Root::default());
        let mut job = config.view(|c| &c.job);

        update(&sender, |c| c.name = "x".to_string());
        update(&sender, |c| c.job.levels = vec![1]);
        let waited = tokio::time::timeout(WAIT, job.changed_by(|c| &c.retry)).await;
        assert!(waited.is_err(), "retry 没变，不应该返回");

        update(&sender, |c| c.job.retry.times = 1);
        tokio::time::timeout(WAIT, job.changed_by(|c| &c.retry))
            .await
            .unwrap()
            .unwrap();
        // 观察过了：再调要等下一次变化
        let waited = tokio::time::timeout(WAIT, job.changed_by(|c| &c.retry)).await;
        assert!(waited.is_err());
    }

    /// changed_by 的基准是上一次观察到的那一版：调用之前已经发生的变化不漏，马上返回
    #[tokio::test]
    async fn changed_by_does_not_miss_earlier_change() {
        let (sender, mut config) = channel(Root::default());
        update(&sender, |c| c.job.retry.times = 1);
        update(&sender, |c| c.name = "x".to_string());
        tokio::time::timeout(WAIT, config.changed_by(|c| &c.job.retry.times))
            .await
            .unwrap()
            .unwrap();

        // changed() 也算观察：之后 changed_by 从那一版比
        update(&sender, |c| c.job.retry.times = 2);
        config.changed().await.unwrap();
        let mut view = config.view(|c| &c.job);
        let waited = tokio::time::timeout(WAIT, view.changed_by(|c| &c.retry.times)).await;
        assert!(waited.is_err(), "times=2 已经观察过");

        // 改过又改回来：按值比较，不算变化
        update(&sender, |c| c.job.retry.times = 9);
        update(&sender, |c| c.job.retry.times = 2);
        let waited = tokio::time::timeout(WAIT, view.changed_by(|c| &c.retry.times)).await;
        assert!(waited.is_err());
    }

    /// clone 的句柄从原句柄观察到的那一版接着算，之后各记各的
    #[tokio::test]
    async fn clone_inherits_seen() {
        let (sender, config) = channel(Root::default());
        update(&sender, |c| c.job.enabled = true);
        let mut cloned = config.clone();
        tokio::time::timeout(WAIT, cloned.changed_by(|c| &c.job.enabled))
            .await
            .unwrap()
            .unwrap();
        let mut original = config;
        tokio::time::timeout(WAIT, original.changed_by(|c| &c.job.enabled))
            .await
            .unwrap()
            .unwrap();
    }

    /// 组件关闭：changed / changed_by 返回 LiveConfigClosed
    #[tokio::test]
    async fn closed_returns_err() {
        let (sender, config) = channel(Root::default());
        let mut view = config.view(|c| &c.job);
        drop(sender);
        let e = view.changed_by(|c| &c.enabled).await.unwrap_err();
        assert!(e.is(RepoErr::LiveConfigClosed), "{e:#}");
        let e = view.changed().await.unwrap_err();
        assert!(e.is(RepoErr::LiveConfigClosed), "{e:#}");
    }

    /// on_change：每次变化回调一次，拿到新值；drop guard 后不再回调
    #[tokio::test]
    async fn on_change_until_guard_dropped() {
        let (sender, config) = channel(Root::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let guard = config.view(|c| &c.job).on_change({
            let seen = seen.clone();
            move |job| seen.lock().unwrap().push(job.retry.times)
        });

        update(&sender, |c| c.job.retry.times = 1);
        eventually(|| *seen.lock().unwrap() == vec![1]).await;
        // 别的地方变了也回调
        update(&sender, |c| c.name = "x".to_string());
        eventually(|| *seen.lock().unwrap() == vec![1, 1]).await;

        drop(guard);
        update(&sender, |c| c.job.retry.times = 2);
        tokio::time::sleep(WAIT).await;
        assert_eq!(*seen.lock().unwrap(), vec![1, 1]);
    }

    /// on_change_by：只有盯的字段变了才回调，参数是字段的新值；回调 panic 后接着监听
    #[tokio::test]
    async fn on_change_by_only_field_and_survives_panic() {
        let (sender, config) = channel(Root::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let _guard = config.on_change_by(|c| &c.job.retry.times, {
            let seen = seen.clone();
            move |times| {
                seen.lock().unwrap().push(*times);
                assert_ne!(*times, 1, "回调里 panic");
            }
        });

        update(&sender, |c| c.name = "x".to_string());
        update(&sender, |c| c.job.retry.times = 1);
        eventually(|| *seen.lock().unwrap() == vec![1]).await;
        update(&sender, |c| c.job.levels = vec![1]);
        update(&sender, |c| c.job.retry.times = 2);
        eventually(|| *seen.lock().unwrap() == vec![1, 2]).await;
    }

    /// 组件关闭后 on_change 的任务自己退出
    #[tokio::test]
    async fn on_change_task_exits_when_closed() {
        let (sender, config) = channel(Root::default());
        let guard = config.on_change(|_| {});
        let guard_by = config.view(|c| &c.job).on_change_by(|c| &c.enabled, |_| {});
        drop(sender);
        eventually(|| guard.task.is_finished() && guard_by.task.is_finished()).await;
    }

    #[derive(Clone, Debug, Default, PartialEq, crate::live_config::hy_live_config)]
    struct DerivedRoot {
        name: String,
        #[live_config(nested)]
        job: DerivedJob,
    }

    #[derive(Clone, Debug, Default, PartialEq, crate::live_config::hy_live_config)]
    struct DerivedJob {
        enabled: bool,
        #[live_config(by_ref)]
        levels: Vec<i32>,
        #[live_config(nested)]
        retry: DerivedRetry,
    }

    #[derive(Clone, Debug, Default, PartialEq, crate::live_config::hy_live_config)]
    struct DerivedRetry {
        times: u32,
    }

    /// `#[derive(hy_live_config)]`：字段方法每次读到最新的值，nested 接着往下，by_ref 和 snapshot 是快照；
    /// 生成的 changed_by、on_change_by 委托给 LiveView
    #[tokio::test]
    async fn derive_reads_latest() {
        let (sender, config) = channel(DerivedRoot::default());
        let root = config.fields();
        let mut job = root.job();
        let retry = job.retry();
        let levels = job.levels();
        let snapshot = job.snapshot();

        update(&sender, |c| {
            c.name = "x".to_string();
            c.job.enabled = true;
            c.job.levels = vec![1, 2];
            c.job.retry.times = 3;
        });
        assert_eq!(root.name(), "x");
        assert!(job.enabled());
        assert_eq!(retry.times(), 3);
        assert_eq!(*job.levels(), vec![1, 2]);
        assert_eq!(root.snapshot().job.retry.times, 3);
        assert!(levels.is_empty(), "by_ref 拿到的是当时的快照");
        assert!(!snapshot.enabled);

        tokio::time::timeout(WAIT, job.changed_by(|c| &c.retry.times))
            .await
            .unwrap()
            .unwrap();
        // retry 句柄还没观察过 times = 3：注册后马上回调一次，之后每变一次回调一次
        let times = Arc::new(Mutex::new(Vec::new()));
        let _guard = retry.on_change_by(|c| &c.times, {
            let times = times.clone();
            move |t| times.lock().unwrap().push(*t)
        });
        eventually(|| *times.lock().unwrap() == vec![3]).await;
        update(&sender, |c| c.job.retry.times = 4);
        eventually(|| *times.lock().unwrap() == vec![3, 4]).await;
    }
}
