//! Redis 锁的端到端测试，要连真实的 Redis：`LOCAL_REDIS_HOST` / `LOCAL_REDIS_PORT`，不设就是 localhost:6379。
//! 手动跑：`cd playground && cargo test --test distributed_lock -- --ignored redis`

use std::time::Duration;

use hygiea::env::{BuiltinKey, env_get_or_else};
use hygiea::redis::{FredRedisPool, RedisConfig};

use super::core::DistributedLock;
use super::redis::RedisLock;

async fn pool(watchdog_secs: u64) -> RedisLock {
    let pool = FredRedisPool::connect(RedisConfig {
        host: env_get_or_else(BuiltinKey::LocalRedisHost, || "localhost".to_string()),
        port: env_get_or_else(BuiltinKey::LocalRedisPort, || "6379".to_string())
            .parse()
            .expect("LOCAL_REDIS_PORT must be a number"),
        ..RedisConfig::default()
    })
    .await
    .unwrap();
    RedisLock {
        pool: (*pool).clone(),
        ttl: Duration::from_secs(watchdog_secs),
    }
}

#[tokio::test]
#[ignore = "requires running Redis"]
async fn redis_lock_tests() {
    try_lock_and_release().await;
    watchdog_keeps_lock_beyond_ttl().await;
}

async fn try_lock_and_release() {
    let (a, b) = (pool(30).await, pool(30).await);
    let guard = a.try_lock("test:redis:basic").await.unwrap().unwrap();
    // 另一个连接池（相当于另一个进程）拿不到
    assert!(b.try_lock("test:redis:basic").await.unwrap().is_none());
    guard.release().await.unwrap();
    let guard = b.try_lock("test:redis:basic").await.unwrap().unwrap();
    guard.release().await.unwrap();
}

/// TTL 1 秒，持有 3 秒：看门狗每 1/3 秒续期，期间别人一直拿不到；释放后立刻能拿
async fn watchdog_keeps_lock_beyond_ttl() {
    let (a, b) = (pool(1).await, pool(1).await);
    let guard = a.try_lock("test:redis:watchdog").await.unwrap().unwrap();
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            b.try_lock("test:redis:watchdog").await.unwrap().is_none(),
            "lock expired while still held; watchdog did not renew"
        );
    }
    guard.release().await.unwrap();
    let guard = b.try_lock("test:redis:watchdog").await.unwrap().unwrap();
    guard.release().await.unwrap();
}
