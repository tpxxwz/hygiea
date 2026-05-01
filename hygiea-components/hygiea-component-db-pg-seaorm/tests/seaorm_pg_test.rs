use hygiea_component_db_pg_seaorm::{SeaOrmPgComponent, SeaOrmPgConfig};
use hygiea_core::app::{Registry, RegistryConfig};
use hygiea_core::env::{BuiltinKey, env_get, env_get_or_else};
use hygiea_core::log::TracingConfig;

fn pg_url() -> String {
    let host = env_get(BuiltinKey::LocalPgHost);
    let port = env_get(BuiltinKey::LocalPgPort);
    let db = env_get_or_else(BuiltinKey::LocalPgDb, || env_get(BuiltinKey::LocalPgUser));
    let user = env_get(BuiltinKey::LocalPgUser);
    let password = env_get(BuiltinKey::LocalPgPassword);
    format!("postgres://{}:{}@{}:{}/{}", user, password, host, port, db)
}

#[tokio::test]
async fn component_fails_when_db_unreachable() {
    let mut registry = Registry::with_config(RegistryConfig {
        tracing: TracingConfig::default(),
    })
    .add::<SeaOrmPgComponent>(SeaOrmPgConfig::default());

    assert!(registry.launch().await.is_err());
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn with_lock_executes_closure() {
    use hygiea_component_db_pg_seaorm::{DistributedLock, PgDb};

    let conn = sea_orm::Database::connect(&pg_url()).await.unwrap();
    let db = PgDb::new(conn, "public".to_string());

    let result = db
        .with_lock(12345, || async { Ok::<_, sea_orm::sqlx::Error>(42) })
        .await;
    assert_eq!(result.unwrap(), 42);
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn try_with_lock_returns_some_when_acquired() {
    use hygiea_component_db_pg_seaorm::{DistributedLock, PgDb};

    let conn = sea_orm::Database::connect(&pg_url()).await.unwrap();
    let db = PgDb::new(conn, "public".to_string());

    let result = db
        .try_with_lock(99999, || async { Ok::<_, sea_orm::sqlx::Error>("done") })
        .await;
    assert!(result.unwrap().is_some());
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn with_lock_serializes_concurrent_access() {
    use hygiea_component_db_pg_seaorm::{DistributedLock, PgDb};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let url = pg_url();
    let db1 = Arc::new(PgDb::new(
        sea_orm::Database::connect(&url).await.unwrap(),
        "public".to_string(),
    ));
    let db2 = Arc::new(PgDb::new(
        sea_orm::Database::connect(&url).await.unwrap(),
        "public".to_string(),
    ));

    let inside = Arc::new(AtomicBool::new(false));
    let conflict = Arc::new(AtomicBool::new(false));

    let (inside1, conflict1) = (inside.clone(), conflict.clone());
    let (inside2, conflict2) = (inside.clone(), conflict.clone());

    let t1 = tokio::spawn(async move {
        db1.with_lock(777i64, || async move {
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
        db2.with_lock(777i64, || async move {
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

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn try_with_lock_returns_none_when_lock_held() {
    use hygiea_component_db_pg_seaorm::{DistributedLock, PgDb};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let url = pg_url();
    let db1 = Arc::new(PgDb::new(
        sea_orm::Database::connect(&url).await.unwrap(),
        "public".to_string(),
    ));
    let db2 = Arc::new(PgDb::new(
        sea_orm::Database::connect(&url).await.unwrap(),
        "public".to_string(),
    ));

    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = barrier.clone();

    let t1 = tokio::spawn(async move {
        db1.with_lock(888i64, || async move {
            barrier.wait().await;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            Ok::<_, sea_orm::sqlx::Error>(())
        })
        .await
        .unwrap()
    });

    barrier_clone.wait().await;

    let result = db2
        .try_with_lock(888i64, || async {
            Ok::<_, sea_orm::sqlx::Error>("should not run")
        })
        .await
        .unwrap();

    assert!(
        result.is_none(),
        "try_with_lock should return None when lock is already held"
    );

    t1.await.unwrap();
}
