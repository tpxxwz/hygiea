//! 对外的句柄：[`LiveConfig`]（绑定拿到的整份配置）、[`LiveView`]（其中一块）、[`LiveRef`]（`LiveView::get` 的快照）

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::watch;

use hygiea_core::{Result, err};

use crate::RepoErr;

/// 绑定到 struct 的配置，`Clone` 后各处共用同一份（`T` 不用实现 `Clone`）
pub struct LiveConfig<T> {
    pub(super) receiver: watch::Receiver<Arc<T>>,
    /// [`fixed`](Self::fixed) 建的才有：留着发送端，`changed()` 一直等不到，而不是马上报后台任务已退出
    fixed: Option<Arc<watch::Sender<Arc<T>>>>,
}

impl<T> Clone for LiveConfig<T> {
    fn clone(&self) -> Self {
        Self {
            receiver: self.receiver.clone(),
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
    /// 连接池 bind 出来的句柄
    pub(super) fn new(receiver: watch::Receiver<Arc<T>>) -> Self {
        Self {
            receiver,
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
        Self {
            receiver,
            fixed: Some(Arc::new(sender)),
        }
    }

    /// 当前最新的值。内部是 tokio `watch` 的读写锁：读锁里只 clone 一下 `Arc`（引用计数 +1）就放开，
    /// 拿到的是这一刻的快照，之后用它不持有锁，也不会被后来的新值改掉。一次处理里要用几个配置项时只 `get()` 一次，
    /// 这几个值就来自同一个版本
    pub fn get(&self) -> Arc<T> {
        self.receiver.borrow().clone()
    }

    /// 等到值变了再返回（相对上一次 `changed` 或 `bind` 时）。后台任务退出了返回 [`RepoErr::LiveConfigClosed`]
    pub async fn changed(&mut self) -> Result<()> {
        self.receiver
            .changed()
            .await
            .map_err(|_| err!(RepoErr::LiveConfigClosed))
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

    /// 等到整份配置变了再返回（不只是这一块变了）。后台任务退出了返回 [`RepoErr::LiveConfigClosed`]
    pub async fn changed(&mut self) -> Result<()> {
        self.source.changed().await
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

/// 抹掉整份配置的类型 `T`：[`LiveView`] 只带 `U`
trait Source<U>: Send + Sync {
    fn get(&self) -> LiveRef<U>;
    fn changed(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
    fn clone_box(&self) -> Box<dyn Source<U>>;
}

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

    fn changed(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(self.config.changed())
    }

    fn clone_box(&self) -> Box<dyn Source<U>> {
        Box::new(Projection {
            config: self.config.clone(),
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

#[cfg(test)]
mod tests {
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

    /// 固定值：get / view 拿到给的值，changed 一直等不到（不会马上报已关闭）
    #[tokio::test]
    async fn fixed_never_changes() {
        let mut config = LiveConfig::fixed(Config {
            sync: Sync { enabled: true },
        });
        assert!(config.get().sync.enabled);
        let mut view = config.view(|c| &c.sync);
        assert_eq!(*view.get(), Sync { enabled: true });

        let waited = tokio::time::timeout(Duration::from_millis(50), config.changed()).await;
        assert!(waited.is_err(), "changed() 不应该返回");
        let waited = tokio::time::timeout(Duration::from_millis(50), view.changed()).await;
        assert!(waited.is_err());
    }
}
