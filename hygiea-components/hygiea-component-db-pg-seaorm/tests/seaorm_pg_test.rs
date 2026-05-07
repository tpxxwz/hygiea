use hygiea_component_db_pg_seaorm::{SeaOrmPgConfig, SeaOrmPgPool};
use hygiea_core::env::{BuiltinKey, env_get, env_get_or_else};

fn cloud_pg_config() -> SeaOrmPgConfig {
    SeaOrmPgConfig {
        host: env_get(BuiltinKey::CloudPgHost),
        port: env_get(BuiltinKey::CloudPgPort)
            .parse()
            .expect("CLOUD_PG_PORT must be a number"),
        username: env_get(BuiltinKey::CloudPgUser),
        password: env_get(BuiltinKey::CloudPgPassword),
        database: env_get_or_else(BuiltinKey::CloudPgDb, || env_get(BuiltinKey::CloudPgUser)),
        params: env_get(BuiltinKey::CloudPgParams),
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
    test_lock_executes_closure(&db).await;
    test_try_lock_returns_some_when_acquired(&db).await;
    test_lock_serializes_concurrent_access(&db).await;
    test_try_lock_returns_none_when_lock_held(&db).await;
    test_concurrent_distinct_keys_under_pool_pressure().await;
}

async fn test_lock_executes_closure(db: &SeaOrmPgPool) {
    use hygiea_component_db_pg_seaorm::DistributedLock;

    let r1 = db
        .lock(12345i32, || async { Ok::<_, sea_orm::sqlx::Error>(42) })
        .await;
    assert_eq!(r1.unwrap(), 42);

    let r2 = db
        .lock((1i32, 2i32), || async { Ok::<_, sea_orm::sqlx::Error>(42) })
        .await;
    assert_eq!(r2.unwrap(), 42);

    let r3 = db
        .lock("test:lock", || async { Ok::<_, sea_orm::sqlx::Error>(42) })
        .await;
    assert_eq!(r3.unwrap(), 42);
}

async fn test_try_lock_returns_some_when_acquired(db: &SeaOrmPgPool) {
    use hygiea_component_db_pg_seaorm::DistributedLock;

    let r1 = db
        .try_lock(99999i32, || async { Ok::<_, sea_orm::sqlx::Error>("done") })
        .await;
    assert!(r1.unwrap().is_some());

    let r2 = db
        .try_lock((9i32, 9i32), || async {
            Ok::<_, sea_orm::sqlx::Error>("done")
        })
        .await;
    assert!(r2.unwrap().is_some());

    let r3 = db
        .try_lock("test:try_lock", || async {
            Ok::<_, sea_orm::sqlx::Error>("done")
        })
        .await;
    assert!(r3.unwrap().is_some());
}

async fn test_lock_serializes_concurrent_access(db: &SeaOrmPgPool) {
    use hygiea_component_db_pg_seaorm::DistributedLock;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let db1 = db.clone();
    let db2 = db.clone();

    let inside = Arc::new(AtomicBool::new(false));
    let conflict = Arc::new(AtomicBool::new(false));

    let (inside1, conflict1) = (inside.clone(), conflict.clone());
    let (inside2, conflict2) = (inside.clone(), conflict.clone());

    let t1 = tokio::spawn(async move {
        db1.lock(777i64, || async move {
            if inside1.swap(true, Ordering::SeqCst) {
                conflict1.store(true, Ordering::SeqCst);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            inside1.store(false, Ordering::SeqCst);
            Ok::<_, sea_orm::sqlx::Error>(())
        })
        .await
        .unwrap()
    });

    let t2 = tokio::spawn(async move {
        db2.lock(777i64, || async move {
            if inside2.swap(true, Ordering::SeqCst) {
                conflict2.store(true, Ordering::SeqCst);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            inside2.store(false, Ordering::SeqCst);
            Ok::<_, sea_orm::sqlx::Error>(())
        })
        .await
        .unwrap()
    });

    t1.await.unwrap();
    t2.await.unwrap();

    assert!(
        !conflict.load(Ordering::SeqCst),
        "two closures ran concurrently under the same lock"
    );
}

async fn test_concurrent_distinct_keys_under_pool_pressure() {
    use hygiea_component_db_pg_seaorm::DistributedLock;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    const TASKS: u32 = 100;
    const MAX_CONNECTIONS: u32 = 50;

    let mut config = cloud_pg_config();
    config.max_connections = Some(MAX_CONNECTIONS);
    let db = SeaOrmPgPool::connect(config).await.unwrap();

    let completed = Arc::new(AtomicU32::new(0));
    let start = std::time::Instant::now();

    let handles: Vec<_> = (0..TASKS)
        .map(|i| {
            let db = db.clone();
            let completed = completed.clone();
            tokio::spawn(async move {
                let key = format!("order_id:{i}");
                db.lock(key, || async {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    Ok::<_, sea_orm::sqlx::Error>(())
                })
                .await
                .unwrap();
                completed.fetch_add(1, Ordering::Relaxed);
            })
        })
        .collect();

    for h in handles {
        h.await.unwrap();
    }

    let elapsed = start.elapsed();
    let count = completed.load(Ordering::Relaxed);
    println!("{count}/{TASKS} locks completed in {elapsed:?} (max_connections={MAX_CONNECTIONS})");
    assert_eq!(count, TASKS);
}

async fn test_try_lock_returns_none_when_lock_held(db: &SeaOrmPgPool) {
    use hygiea_component_db_pg_seaorm::DistributedLock;
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let db1 = db.clone();
    let db2 = db.clone();

    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = barrier.clone();

    let t1 = tokio::spawn(async move {
        db1.lock(888i64, || async move {
            barrier.wait().await;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            Ok::<_, sea_orm::sqlx::Error>(())
        })
        .await
        .unwrap()
    });

    barrier_clone.wait().await;

    let result = db2
        .try_lock(888i64, || async {
            Ok::<_, sea_orm::sqlx::Error>("should not run")
        })
        .await
        .unwrap();

    assert!(
        result.is_none(),
        "try_lock should return None when lock is already held"
    );

    t1.await.unwrap();
}
