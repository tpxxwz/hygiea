//! 进程内的锁（feature `lock-tokio`）：[`TokioLock`]，不依赖外部服务。
//!
//! 一个 [`TokioLock`]（和它的 clone）共用一张 key → `tokio::sync::Mutex` 的表，key 精确匹配；
//! 别的进程、另外建的 [`TokioLock`] 都看不到这些锁。只实现字符串 key，整数 key 走默认实现转成字符串 key。
//! 阻塞版直接等 Mutex（先到先得），不轮询

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::OwnedMutexGuard;

use super::{DistributedLock, LockGuard, LockKey};
use crate::Result;

type Entries = DashMap<String, Arc<tokio::sync::Mutex<()>>>;

/// 进程内的锁，`TokioLock::default()` 建一个，要共用就 clone：clone 出来的共用同一张表
///
/// ```ignore
/// let lock = TokioLock::default();
/// if let Some(guard) = lock.try_lock(lock_key!(JobLockKey::Cleanup)).await? {
///     run_job().await?;
///     guard.release().await?;
/// }
/// ```
#[derive(Clone, Debug, Default)]
pub struct TokioLock {
    /// 正在被持有或等待的 key；没人用了就删掉，表不会随 key 的个数一直长
    entries: Arc<Entries>,
}

#[async_trait]
impl DistributedLock for TokioLock {
    async fn try_lock(&self, key: LockKey) -> Result<Option<LockGuard>> {
        let name = key.as_str()?.to_string();
        let entry = self.entry(&name);
        Ok(match entry.try_lock_owned() {
            Ok(guard) => Some(self.guard(name, guard)),
            Err(_) => None,
        })
    }

    async fn lock(&self, key: LockKey, timeout: Duration) -> Result<Option<LockGuard>> {
        if timeout.is_zero() {
            return self.try_lock(key).await;
        }
        let name = key.as_str()?.to_string();
        // 超时、调用方把这个 future 丢掉（select!、外层 timeout）都会走到它的 drop：等待的 future 先丢掉，
        // 这时没人持有、没人在等就从表里删。拿到锁时 guard 还引用着，不会删
        let _cleanup = Cleanup {
            lock: self,
            name: &name,
        };
        let entry = self.entry(&name);
        match tokio::time::timeout(timeout, entry.lock_owned()).await {
            Ok(guard) => Ok(Some(self.guard(name.clone(), guard))),
            Err(_) => Ok(None),
        }
    }
}

impl TokioLock {
    /// 取 key 对应的 Mutex，没有就建。clone 在 key 所在分片的写锁里做，`remove_unused` 看到的引用数才可靠
    fn entry(&self, name: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.entries.entry(name.to_string()).or_default().clone()
    }

    /// 只剩表里那一份引用（没人持有、没人在等）就删掉；检查和删除在同一个分片写锁里
    fn remove_unused(&self, name: &str) {
        self.entries
            .remove_if(name, |_, entry| Arc::strong_count(entry) == 1);
    }

    fn guard(&self, name: String, guard: OwnedMutexGuard<()>) -> LockGuard {
        let held = Held {
            lock: self.clone(),
            name: name.clone(),
            guard: Some(guard),
        };
        LockGuard::new(name, async move {
            drop(held);
            Ok(())
        })
    }
}

/// 等锁结束（拿到、超时、被取消）时看要不要从表里删
struct Cleanup<'a> {
    lock: &'a TokioLock,
    name: &'a str,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        self.lock.remove_unused(self.name);
    }
}

/// 持有中的锁：drop 时先放锁，再看要不要从表里删。不在 tokio 运行时里 drop `LockGuard` 时，
/// 释放的 future 不执行、直接被丢掉，也会走到这里
struct Held {
    lock: TokioLock,
    name: String,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for Held {
    fn drop(&mut self) {
        drop(self.guard.take());
        self.lock.remove_unused(&self.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{hy_lock_key, lock_key};

    #[derive(hy_lock_key)]
    #[lock_internal_module_prefix = "092"]
    enum TestLockKey {
        #[lock_key(code = "001", tpl = "job:{{ id }}")]
        Job,
    }

    fn entries(lock: &TokioLock) -> usize {
        lock.entries.len()
    }

    /// 同一个 key 互斥，不同 key 互不影响；释放后能再拿，表里不留
    #[tokio::test]
    async fn same_key_excludes_until_released() {
        let lock = TokioLock::default();
        let held = lock
            .try_lock(lock_key!(TestLockKey::Job, 1))
            .await
            .unwrap()
            .unwrap();
        assert!(
            lock.try_lock(lock_key!(TestLockKey::Job, 1))
                .await
                .unwrap()
                .is_none()
        );
        let other = lock
            .try_lock(lock_key!(TestLockKey::Job, 2))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entries(&lock), 2);

        held.release().await.unwrap();
        other.release().await.unwrap();
        assert_eq!(entries(&lock), 0);
        let again = lock.try_lock(lock_key!(TestLockKey::Job, 1)).await.unwrap();
        assert!(again.is_some());
    }

    /// clone 出来的共用同一张表；整数 key 走默认实现
    #[tokio::test]
    async fn clones_share_locks() {
        let lock = TokioLock::default();
        let _held = lock.try_lock_i64(42).await.unwrap().unwrap();
        let cloned = lock.clone();
        assert!(cloned.try_lock_i64(42).await.unwrap().is_none());
        assert!(cloned.try_lock_i64(43).await.unwrap().is_some());
    }

    /// 阻塞版：等到别人释放；超时返回 None，表里不留
    #[tokio::test]
    async fn lock_waits_or_times_out() {
        let lock = TokioLock::default();
        let held = lock
            .try_lock(lock_key!(TestLockKey::Job, 1))
            .await
            .unwrap()
            .unwrap();

        let timed_out = lock
            .lock(lock_key!(TestLockKey::Job, 1), Duration::from_millis(20))
            .await
            .unwrap();
        assert!(timed_out.is_none());

        let waiter = {
            let lock = lock.clone();
            tokio::spawn(async move {
                lock.lock(lock_key!(TestLockKey::Job, 1), Duration::from_secs(5))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        held.release().await.unwrap();
        let got = waiter.await.unwrap().unwrap().unwrap();
        got.release().await.unwrap();
        assert_eq!(entries(&lock), 0);
    }

    /// 调用方从外面取消等待（这里用外层 timeout）：持有者先释放、等待者再被取消，表里也不留
    #[tokio::test]
    async fn cancelled_wait_leaves_no_entry() {
        let lock = TokioLock::default();
        let held = lock
            .try_lock(lock_key!(TestLockKey::Job, 1))
            .await
            .unwrap()
            .unwrap();
        let outer = tokio::time::timeout(
            Duration::from_millis(20),
            lock.lock(lock_key!(TestLockKey::Job, 1), Duration::from_secs(5)),
        );
        assert!(outer.await.is_err());
        held.release().await.unwrap();
        assert_eq!(entries(&lock), 0);

        // 持有者释放时等待者还在（锁直接交给等待者），等待者再被取消
        let held = lock
            .try_lock(lock_key!(TestLockKey::Job, 1))
            .await
            .unwrap()
            .unwrap();
        let mut waiting =
            Box::pin(lock.lock(lock_key!(TestLockKey::Job, 1), Duration::from_secs(5)));
        assert!(futures_poll_once(&mut waiting).await.is_none());
        held.release().await.unwrap();
        drop(waiting);
        assert_eq!(entries(&lock), 0);
    }

    /// poll 一次，没完成就返回 None
    async fn futures_poll_once<F: std::future::Future + Unpin>(f: &mut F) -> Option<F::Output> {
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(match std::pin::Pin::new(&mut *f).poll(cx) {
                std::task::Poll::Ready(v) => Some(v),
                std::task::Poll::Pending => None,
            })
        })
        .await
    }

    /// 没调 release 直接 drop：后台释放，之后能再拿
    #[tokio::test]
    async fn dropped_guard_releases() {
        let lock = TokioLock::default();
        drop(lock.try_lock(lock_key!(TestLockKey::Job, 1)).await.unwrap());
        tokio::task::yield_now().await;
        assert!(
            lock.try_lock(lock_key!(TestLockKey::Job, 1))
                .await
                .unwrap()
                .is_some()
        );
    }
}
