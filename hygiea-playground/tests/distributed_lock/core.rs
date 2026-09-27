//! 锁的抽象：错误、guard、trait。原来是 `hygiea::sync` 下的 `BaseLockErr`、`LockGuard`、`DistributedLock`

use std::future::Future;
use std::pin::Pin;

use hygiea::app::async_trait;
use hygiea::{HyErr, hy_err};

/// 分布式锁的错误。和 `hygiea::BaseErr` 共用项目前缀 999（playground 的 Cargo.toml 里配成 999，保持原来的错误码），模块前缀是 04；
/// 后端的原始错误挂在 source 上
#[derive(hy_err)]
#[err_code_module_prefix = "04"]
pub enum BaseLockErr {
    /// 加锁时后端出错（连不上、命令失败），不是"锁被别人占着"：占着的情况 `try_lock` 返回 `Ok(None)`
    #[error(err_code = "001", err_tpl = "Acquire lock failed: {{ key }}")]
    AcquireFailed,
    /// 释放锁时后端出错。锁最终还是会因为 TTL 过期或连接断开而释放
    #[error(err_code = "002", err_tpl = "Release lock failed: {{ key }}")]
    ReleaseFailed,
}

type ReleaseFuture = Pin<Box<dyn Future<Output = Result<(), HyErr>> + Send>>;

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
    /// 给后端实现用：`release` 是释放锁的 future（比如回滚事务、执行释放脚本），
    /// 这里只存着，释放时才执行
    pub fn new(
        key: impl Into<String>,
        release: impl Future<Output = Result<(), HyErr>> + Send + 'static,
    ) -> Self {
        Self {
            key: key.into(),
            release: Some(Box::pin(release)),
        }
    }

    /// 锁的 key
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 释放锁，返回后端释放的结果
    pub async fn release(mut self) -> Result<(), HyErr> {
        match self.release.take() {
            Some(release) => release.await,
            None => Ok(()),
        }
    }
}

impl Drop for LockGuard {
    /// 没调 `release` 就丢掉：交给 tokio 在后台释放。不在 tokio 运行时里（极少见）就没法执行释放，
    /// 只能靠后端兜底（redis TTL 过期、数据库连接断开）
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
                "lock {key} dropped outside tokio runtime, it will be released by the backend \
                 (TTL expiry or connection close)"
            ),
        }
    }
}

/// 分布式锁，只有不等待的 [`try_lock`](Self::try_lock)
#[async_trait]
pub trait DistributedLock: Send + Sync {
    /// 尝试加锁，拿不到（被别人占着，包括被自己持有）立即返回 `Ok(None)`，不可重入；
    /// 后端出错返回 [`BaseLockErr::AcquireFailed`]
    async fn try_lock(&self, key: &str) -> Result<Option<LockGuard>, HyErr>;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    /// 进程内的假锁：记录释放了几次，用来测 guard 和默认的 lock
    #[derive(Default)]
    struct FakeLock {
        held: Arc<Mutex<Vec<String>>>,
        released: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl DistributedLock for FakeLock {
        async fn try_lock(&self, key: &str) -> Result<Option<LockGuard>, HyErr> {
            let mut held = self.held.lock().unwrap();
            if held.iter().any(|k| k == key) {
                return Ok(None);
            }
            held.push(key.to_string());
            let (held, released, key_owned) = (
                Arc::clone(&self.held),
                Arc::clone(&self.released),
                key.to_string(),
            );
            Ok(Some(LockGuard::new(key, async move {
                held.lock().unwrap().retain(|k| k != &key_owned);
                released.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })))
        }
    }

    #[tokio::test]
    async fn release_frees_the_key() {
        let lock = FakeLock::default();
        let guard = lock.try_lock("a").await.unwrap().unwrap();
        assert_eq!(guard.key(), "a");
        assert!(lock.try_lock("a").await.unwrap().is_none());
        assert!(lock.try_lock("b").await.unwrap().is_some());
        guard.release().await.unwrap();
        assert!(lock.try_lock("a").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn dropped_guard_releases_in_background() {
        let lock = FakeLock::default();
        drop(lock.try_lock("a").await.unwrap().unwrap());
        // 后台释放是 spawn 出去的，让它跑一下
        tokio::task::yield_now().await;
        assert_eq!(lock.released.load(Ordering::SeqCst), 1);
        assert!(lock.try_lock("a").await.unwrap().is_some());
    }
}
