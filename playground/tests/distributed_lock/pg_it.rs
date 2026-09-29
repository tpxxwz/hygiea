//! SeaORM + PostgreSQL 锁的端到端测试，要连真实的 PostgreSQL（`CLOUD_PG_*` 环境变量）。
//! 手动跑：`cd playground && cargo test --test distributed_lock -- --ignored pg_it`

use hygiea::db::{SeaOrmPgConfig, SeaOrmPgPool};
use hygiea::env::{BuiltinKey, env_get, env_get_or_else};

fn cloud_pg_config() -> SeaOrmPgConfig {
    SeaOrmPgConfig {
        host: env_get(BuiltinKey::CloudPgHost).unwrap(),
        port: env_get(BuiltinKey::CloudPgPort)
            .unwrap()
            .parse()
            .expect("CLOUD_PG_PORT must be a number"),
        username: env_get(BuiltinKey::CloudPgUser).unwrap(),
        password: env_get(BuiltinKey::CloudPgPassword).unwrap(),
        database: env_get_or_else(BuiltinKey::CloudPgDb, || {
            env_get(BuiltinKey::CloudPgUser).unwrap()
        }),
        params: env_get(BuiltinKey::CloudPgParams).unwrap(),
        schema_search_path: "dict_jp".to_string(),
        sqlx_logging_level: "warn".to_string(),
        ..SeaOrmPgConfig::default()
    }
}

async fn make_db() -> SeaOrmPgPool {
    SeaOrmPgPool::connect(cloud_pg_config()).await.unwrap()
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn distributed_lock_tests() {
    let db = make_db().await;
    test_try_lock_and_release(&db).await;
    test_try_lock_returns_none_when_held(&db).await;
    test_try_lock_is_exclusive_under_concurrency(&db).await;
    test_concurrent_distinct_keys_under_pool_pressure().await;
}

async fn test_try_lock_and_release(db: &SeaOrmPgPool) {
    use super::core::DistributedLock;

    let guard = db.try_lock("test:try_lock").await.unwrap().unwrap();
    assert_eq!(guard.key(), "test:try_lock");
    guard.release().await.unwrap();
    // 释放后能再拿到
    let guard = db.try_lock("test:try_lock").await.unwrap().unwrap();
    guard.release().await.unwrap();
}

async fn test_try_lock_returns_none_when_held(db: &SeaOrmPgPool) {
    use super::core::DistributedLock;

    let held = db.try_lock("test:held").await.unwrap().unwrap();
    // 同一个 key 在别的连接上拿不到，别的 key 不受影响
    assert!(db.try_lock("test:held").await.unwrap().is_none());
    let other = db.try_lock("test:other").await.unwrap().unwrap();
    other.release().await.unwrap();
    held.release().await.unwrap();

    // guard 直接丢掉也会释放（后台回滚事务）
    drop(db.try_lock("test:dropped").await.unwrap().unwrap());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let again = db.try_lock("test:dropped").await.unwrap().unwrap();
    again.release().await.unwrap();
}

/// 多个任务同时 try_lock 同一个 key：拿到的任务之间不会重叠，至少有一个拿到
async fn test_try_lock_is_exclusive_under_concurrency(db: &SeaOrmPgPool) {
    use super::core::DistributedLock;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;

    let inside = Arc::new(AtomicBool::new(false));
    let conflict = Arc::new(AtomicBool::new(false));
    let acquired = Arc::new(AtomicU32::new(0));

    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let (db, inside, conflict, acquired) = (
                db.clone(),
                inside.clone(),
                conflict.clone(),
                acquired.clone(),
            );
            tokio::spawn(async move {
                let Some(guard) = db.try_lock("test:exclusive").await.unwrap() else {
                    return;
                };
                acquired.fetch_add(1, Ordering::SeqCst);
                if inside.swap(true, Ordering::SeqCst) {
                    conflict.store(true, Ordering::SeqCst);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                inside.store(false, Ordering::SeqCst);
                guard.release().await.unwrap();
            })
        })
        .collect();
    for task in tasks {
        task.await.unwrap();
    }

    assert!(
        !conflict.load(Ordering::SeqCst),
        "two tasks held the same lock at the same time"
    );
    assert!(acquired.load(Ordering::SeqCst) >= 1);
}

/// 100 个不同的 key 并发加锁，连接池只有 50 个连接：持有锁要占连接，超出的在拿连接时排队，
/// 前面的释放后接着拿，最终都能完成
async fn test_concurrent_distinct_keys_under_pool_pressure() {
    use super::core::DistributedLock;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    const TASKS: u32 = 100;
    const MAX_CONNECTIONS: u32 = 50;

    let mut config = cloud_pg_config();
    config.max_connections = Some(MAX_CONNECTIONS);
    let db = SeaOrmPgPool::connect(config).await.unwrap();

    let completed = Arc::new(AtomicU32::new(0));
    let start = std::time::Instant::now();

    let handles: Vec<_> = (0..TASKS)
        .map(|i| {
            let (db, completed) = (db.clone(), completed.clone());
            tokio::spawn(async move {
                let guard = db
                    .try_lock(&format!("order_id:{i}"))
                    .await
                    .unwrap()
                    .expect("distinct keys never conflict");
                tokio::time::sleep(Duration::from_millis(300)).await;
                guard.release().await.unwrap();
                completed.fetch_add(1, Ordering::Relaxed);
            })
        })
        .collect();
    for handle in handles {
        handle.await.unwrap();
    }

    let elapsed = start.elapsed();
    let count = completed.load(Ordering::Relaxed);
    println!("{count}/{TASKS} locks completed in {elapsed:?} (max_connections={MAX_CONNECTIONS})");
    assert_eq!(count, TASKS);
}
