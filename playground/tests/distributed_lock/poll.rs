//! 轮询等待：原来是 `DistributedLock::lock` 的默认实现：轮询 `try_lock` 直到超时。core 只保留不等待的
//! `try_lock`，因为轮询在高竞争下有这些问题：锁释放后最多 150ms 才发现、不公平（先来的不一定先拿到）、
//! 每个等待者每秒重试 7～20 次打到后端、单次 `try_lock` 慢时总耗时会超过 `timeout`。
//! 各后端更好的等待方式：sqlite 用 tokio `Mutex` + `timeout`；pg 用阻塞版 `pg_advisory_xact_lock` +
//! `lock_timeout`（等待期间占连接，要防连接池被占满）；redis 参考 Redisson 的 pub/sub 通知。

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::core::{DistributedLock, LockGuard};
use hygiea::Result;
use hygiea::app::async_trait;

// ========== 轮询等待 ==========

/// 最多等 `timeout`，拿不到返回 `Ok(None)`；`timeout` 为 0 时和 `try_lock` 一样。
///
/// 轮询 `try_lock`，每次间隔 50～150 毫秒（随机，避免多个实例同时重试），
/// 等待用 async sleep，不占线程也不占数据库连接
pub async fn lock_with_timeout<L: DistributedLock + ?Sized>(
    lock: &L,
    key: &str,
    timeout: Duration,
) -> Result<Option<LockGuard>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(guard) = lock.try_lock(key).await? {
            return Ok(Some(guard));
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(retry_interval().min(deadline - now)).await;
    }
}

/// 50～150 毫秒之间的随机间隔。不引入随机数依赖：`RandomState` 每次创建的种子都是随机的
fn retry_interval() -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    Duration::from_millis(50 + random % 101)
}

// ========== 测试 ==========

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程内的假锁：记录释放了几次
    #[derive(Default)]
    struct FakeLock {
        held: Arc<Mutex<Vec<String>>>,
        released: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl DistributedLock for FakeLock {
        async fn try_lock(&self, key: &str) -> Result<Option<LockGuard>> {
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

    #[tokio::test(start_paused = true)]
    async fn lock_waits_until_released_or_timeout() {
        let lock = Arc::new(FakeLock::default());
        let guard = lock.try_lock("a").await.unwrap().unwrap();

        // 超时前一直拿不到：返回 None
        assert!(
            lock_with_timeout(&*lock, "a", Duration::from_millis(500))
                .await
                .unwrap()
                .is_none()
        );

        // 别人 200ms 后释放：等得到
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            guard.release().await.unwrap();
        });
        assert!(
            lock_with_timeout(&*lock, "a", Duration::from_secs(2))
                .await
                .unwrap()
                .is_some()
        );
        releaser.await.unwrap();
        assert_eq!(lock.released.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn zero_timeout_is_try_lock() {
        let lock = FakeLock::default();
        let _guard = lock.try_lock("a").await.unwrap().unwrap();
        assert!(
            lock_with_timeout(&lock, "a", Duration::ZERO)
                .await
                .unwrap()
                .is_none()
        );
    }
}
