//! 持有中的锁

use std::future::Future;
use std::pin::Pin;

use crate::Result;

type ReleaseFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// 持有中的锁。`release().await` 释放锁并拿到结果（推荐）；没调 `release` 就 drop 的话，
/// 在后台释放，失败只打 WARN。
///
/// 后端实现时用 [`LockGuard::new`] 传入"怎么释放"：一个还没开始执行的 future，
/// 调 `release` 或 drop 时才执行
#[must_use = "持有锁要留着 guard，用完调 release().await；直接丢掉会立刻释放锁"]
pub struct LockGuard {
    key: String,
    release: Option<ReleaseFuture>,
}

impl LockGuard {
    /// 给后端实现用：`key` 是日志、错误里显示的 key，`release` 是释放锁的 future（比如回滚事务、执行释放脚本），
    /// 这里只存着，释放时才执行
    pub fn new(
        key: impl Into<String>,
        release: impl Future<Output = Result<()>> + Send + 'static,
    ) -> Self {
        Self {
            key: key.into(),
            release: Some(Box::pin(release)),
        }
    }

    /// 锁的 key，整数 key 由后端格式化
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 释放锁，返回后端释放的结果
    pub async fn release(mut self) -> Result<()> {
        match self.release.take() {
            Some(release) => release.await,
            None => Ok(()),
        }
    }
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard").field("key", &self.key).finish()
    }
}

impl Drop for LockGuard {
    /// 没调 `release` 就丢掉：交给 tokio 在后台释放。不在 tokio 运行时里（极少见）就没法执行释放，
    /// 只能靠后端兜底（事务结束、连接断开、TTL 过期）
    fn drop(&mut self) {
        let Some(release) = self.release.take() else {
            return;
        };
        let key = std::mem::take(&mut self.key);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    if let Err(e) = release.await {
                        tracing::warn!("release lock {key} in background failed: {e:#}");
                    }
                });
            }
            Err(_) => tracing::warn!(
                "lock {key} dropped outside tokio runtime, it will be released by the backend"
            ),
        }
    }
}
