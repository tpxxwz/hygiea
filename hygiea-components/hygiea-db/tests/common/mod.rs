//! 几个连接池的集成测试共用的部分：`mod common;` 引进去用。每个测试文件只用到其中一部分，没用到的不算警告

#![allow(dead_code)]

use std::future::Future;
use std::time::Duration;

#[cfg(feature = "lock")]
use hygiea_core::lock::{DistributedLock, LockKey};

/// 条件在 `secs` 秒内成立
pub async fn eventually<F, Fut>(secs: u64, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{secs} 秒内条件没有成立"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---- 锁：和连接池类型无关的检查，`a`、`b` 模拟两个实例 -------------------------------------

/// i64：别的实例拿不到，自己再拿也拿不到（不可重入），release 后别人能拿到
#[cfg(feature = "lock")]
pub async fn i64_is_exclusive<L: DistributedLock>(a: &L, b: &L) {
    let guard = a.try_lock_i64(1).await.unwrap().unwrap();
    assert_eq!(guard.key(), "advisory 1");
    assert!(b.try_lock_i64(1).await.unwrap().is_none());
    assert!(a.try_lock_i64(1).await.unwrap().is_none());
    assert!(b.try_lock_i64(2).await.unwrap().is_some());
    guard.release().await.unwrap();
    assert!(b.try_lock_i64(1).await.unwrap().is_some());
}

/// 阻塞版：超时返回 None，等的时间不短于 timeout；别人释放了就拿到
#[cfg(feature = "lock")]
pub async fn blocking_waits_and_times_out<L: DistributedLock + Clone + 'static>(a: &L, b: &L) {
    let guard = a.try_lock_i64(1).await.unwrap().unwrap();
    let start = tokio::time::Instant::now();
    assert!(
        b.lock_i64(1, Duration::from_millis(300))
            .await
            .unwrap()
            .is_none()
    );
    assert!(start.elapsed() >= Duration::from_millis(300));
    let b = b.clone();
    let waiter = tokio::spawn(async move { b.lock_i64(1, Duration::from_secs(10)).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    guard.release().await.unwrap();
    assert!(waiter.await.unwrap().unwrap().is_some());
}

/// 字符串 key：互斥，release 后别人能拿到
#[cfg(feature = "lock")]
pub async fn string_key_is_exclusive<L: DistributedLock>(a: &L, b: &L, key: impl Fn() -> LockKey) {
    let guard = a.try_lock(key()).await.unwrap().unwrap();
    assert!(b.try_lock(key()).await.unwrap().is_none());
    guard.release().await.unwrap();
    assert!(
        b.lock(key(), Duration::from_secs(5))
            .await
            .unwrap()
            .is_some()
    );
}
