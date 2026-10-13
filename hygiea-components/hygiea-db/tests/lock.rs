//! 连接池的分布式锁（feature `lock`、`lock-row`）的集成测试。
//!
//! PostgreSQL，是 container 层，要 Docker。字符串 key 开不开 `lock-row` 实现不一样，两种都要跑：
//! ```sh
//! cargo test -p hygiea-db --all-features --test lock -- --ignored ::container::
//! cargo test -p hygiea-db --features sqlx,seaorm,postgres,lock --test lock -- --ignored ::container::
//! ```

#![cfg(all(
    feature = "lock",
    feature = "sqlx",
    feature = "seaorm",
    feature = "postgres"
))]

use std::future::Future;
use std::time::Duration;

use hygiea_core::lock::{DistributedLock, hy_lock_key, hy_lock_type, lock_key};
use hygiea_test::container;

#[derive(hy_lock_type)]
#[lock_internal_module_prefix = "100"]
enum TestLockType {
    #[lock_type(code = "001")]
    Job,
}

#[derive(hy_lock_key)]
#[lock_internal_module_prefix = "100"]
enum TestLockKey {
    #[lock_key(code = "002", tpl = "job:{{ id }}")]
    Job,
}

/// 等条件成立，最多等 `secs` 秒
async fn eventually<F, Fut>(secs: u64, mut check: F)
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

/// 用代码启动的官方 postgres 镜像
#[container]
mod postgres {
    use hygiea_core::app::{ConfigResource, RegistryConfig};
    use hygiea_db::{PgConnectConfig, SeaOrmPgConfig, SeaOrmPgPool, SqlxPgConfig, SqlxPgPool};
    #[cfg(feature = "lock-row")]
    use hygiea_db::{PoolConfig, RepoErr};
    use hygiea_test::container::{ContainerSpec, RunningContainer};

    use super::*;

    const PASSWORD: &str = "hygiea-test";
    const PORT: u16 = 5432;

    async fn start() -> RunningContainer {
        ContainerSpec::new("postgres", "18")
            .env("POSTGRES_PASSWORD", PASSWORD)
            .port(PORT)
            .wait_log_times("database system is ready to accept connections", 2)
            .start()
            .await
    }

    /// 应用名 `hy`：锁表是 hy_lock
    fn app(name: &str) -> RegistryConfig {
        RegistryConfig {
            app_name: name.to_string(),
            ..Default::default()
        }
    }

    fn connect(server: &RunningContainer) -> PgConnectConfig {
        PgConnectConfig {
            host: server.host().to_string(),
            port: server.port(PORT),
            password: PASSWORD.to_string(),
            ..Default::default()
        }
    }

    /// 一个连接池模拟一个实例
    async fn pool(server: &RunningContainer) -> SqlxPgPool {
        let config = SqlxPgConfig {
            connect: connect(server),
            ..Default::default()
        };
        SqlxPgPool::from_config(&config, &app("hy")).await.unwrap()
    }

    #[cfg(feature = "lock-row")]
    async fn table_exists(pool: &SqlxPgPool, table: &str) -> bool {
        sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(&**pool)
            .await
            .unwrap()
    }

    /// i64：别的实例拿不到，自己再拿也拿不到（不可重入），release 后别人能拿到
    #[tokio::test]
    async fn i64_is_exclusive_until_released() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        let guard = a.try_lock_i64(1).await.unwrap().unwrap();
        assert_eq!(guard.key(), "advisory 1");
        assert!(b.try_lock_i64(1).await.unwrap().is_none());
        assert!(a.try_lock_i64(1).await.unwrap().is_none());
        assert!(b.try_lock_i64(2).await.unwrap().is_some());
        guard.release().await.unwrap();
        assert!(b.try_lock_i64(1).await.unwrap().is_some());
    }

    /// guard 没 release 就 drop：后台回滚事务，锁释放
    #[tokio::test]
    async fn dropped_guard_releases() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        drop(a.try_lock_i64(1).await.unwrap().unwrap());
        eventually(5, || async { b.try_lock_i64(1).await.unwrap().is_some() }).await;
    }

    /// 锁类型 + i32 用原生的 (int, int)：和 i64 是两个 key 空间
    #[tokio::test]
    async fn pair_is_native_and_separate_from_i64() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        let pair = a
            .try_lock_i32(TestLockType::Job, 42)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pair.key(), "advisory 999100001,42");
        assert!(
            b.try_lock_i32(TestLockType::Job, 42)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            b.try_lock_i32(TestLockType::Job, 43)
                .await
                .unwrap()
                .is_some()
        );
        // 同样的数值用 i64 是另一把锁
        assert!(b.try_lock_i64(42).await.unwrap().is_some());

        let held: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND classid = 999100001 \
             AND objid = 42 AND objsubid = 2",
        )
        .fetch_one(&*b)
        .await
        .unwrap();
        assert_eq!(held, 1, "pg_locks 里看到的是原生 (int, int) 形式");
        pair.release().await.unwrap();
    }

    /// 阻塞版：超时返回 None，等的时间不短于 timeout；别人释放了就拿到
    #[tokio::test]
    async fn blocking_waits_and_times_out() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        let guard = a.try_lock_i64(1).await.unwrap().unwrap();

        let start = tokio::time::Instant::now();
        assert!(
            b.lock_i64(1, Duration::from_millis(300))
                .await
                .unwrap()
                .is_none()
        );
        assert!(start.elapsed() >= Duration::from_millis(300));
        assert!(b.lock_i64(1, Duration::ZERO).await.unwrap().is_none());

        let waiter = tokio::spawn(async move { b.lock_i64(1, Duration::from_secs(10)).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        guard.release().await.unwrap();
        let got = waiter.await.unwrap().unwrap();
        assert_eq!(
            got.map(|g| g.key().to_string()).as_deref(),
            Some("advisory 1")
        );

        // (int, int) 的阻塞版
        let held = a.try_lock_i32(TestLockType::Job, 7).await.unwrap().unwrap();
        let c = pool(&server).await;
        assert!(
            c.lock_i32(TestLockType::Job, 7, Duration::from_millis(200))
                .await
                .unwrap()
                .is_none()
        );
        held.release().await.unwrap();
        assert!(
            c.lock_i32(TestLockType::Job, 7, Duration::from_secs(5))
                .await
                .unwrap()
                .is_some()
        );
    }

    /// 字符串 key：启动时建了锁表，key 一行，行锁互斥；阻塞版超时、等到释放
    #[cfg(feature = "lock-row")]
    #[tokio::test]
    async fn string_key_uses_lock_table() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        assert!(table_exists(&a, "hy_lock").await, "启动时建表");

        let guard = a
            .try_lock(lock_key!(TestLockKey::Job, 1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(guard.key(), "999100002:job:1");
        assert!(
            b.try_lock(lock_key!(TestLockKey::Job, 1))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            b.try_lock(lock_key!(TestLockKey::Job, 2))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            b.lock(lock_key!(TestLockKey::Job, 1), Duration::from_millis(200))
                .await
                .unwrap()
                .is_none()
        );

        let waiter = tokio::spawn(async move {
            b.lock(lock_key!(TestLockKey::Job, 1), Duration::from_secs(10))
                .await
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        guard.release().await.unwrap();
        assert!(waiter.await.unwrap().unwrap().is_some());

        let keys: Vec<String> = sqlx::query_scalar("SELECT key FROM hy_lock ORDER BY key")
            .fetch_all(&*a)
            .await
            .unwrap();
        assert_eq!(keys, ["999100002:job:1", "999100002:job:2"]);
    }

    /// 没开 lock-row：字符串 key 哈希成 i64 走 advisory lock，不建表；
    /// 和直接传同一个数字的 try_lock_i64 是同一把锁；阻塞版超时、等到释放
    #[cfg(not(feature = "lock-row"))]
    #[tokio::test]
    async fn string_key_falls_back_to_hashed_advisory() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        let hashed = lock_key!(TestLockKey::Job, 1).hashed_i64().unwrap().get();

        let guard = a
            .try_lock(lock_key!(TestLockKey::Job, 1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            guard.key(),
            format!("advisory {hashed} (hashed 999100002:job:1)")
        );
        assert!(
            b.try_lock(lock_key!(TestLockKey::Job, 1))
                .await
                .unwrap()
                .is_none()
        );
        assert!(b.try_lock_i64(hashed).await.unwrap().is_none());
        assert!(
            b.try_lock(lock_key!(TestLockKey::Job, 2))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            b.lock(lock_key!(TestLockKey::Job, 1), Duration::from_millis(200))
                .await
                .unwrap()
                .is_none()
        );

        let waiter = tokio::spawn(async move {
            b.lock(lock_key!(TestLockKey::Job, 1), Duration::from_secs(10))
                .await
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        guard.release().await.unwrap();
        assert!(waiter.await.unwrap().unwrap().is_some());

        let table: bool = sqlx::query_scalar("SELECT to_regclass('hy_lock') IS NOT NULL")
            .fetch_one(&*a)
            .await
            .unwrap();
        assert!(!table, "没开 lock-row 不建锁表");
    }

    /// SeaORM 连接池用底层的 sqlx 池，和 sqlx 连接池互斥
    #[tokio::test]
    async fn seaorm_pool_shares_the_locks() {
        let server = start().await;
        let sqlx_pool = pool(&server).await;
        let config = SeaOrmPgConfig {
            connect: connect(&server),
            ..Default::default()
        };
        let seaorm = SeaOrmPgPool::from_config(&config, &app("hy"))
            .await
            .unwrap();
        let guard = seaorm.try_lock_i64(5).await.unwrap().unwrap();
        assert!(sqlx_pool.try_lock_i64(5).await.unwrap().is_none());
        let key = seaorm
            .try_lock(lock_key!(TestLockKey::Job, 9))
            .await
            .unwrap()
            .unwrap();
        assert!(
            sqlx_pool
                .try_lock(lock_key!(TestLockKey::Job, 9))
                .await
                .unwrap()
                .is_none()
        );
        guard.release().await.unwrap();
        key.release().await.unwrap();
    }

    /// connect_lazy：启动时不连库、不建表，第一次用字符串 key 时建
    #[cfg(feature = "lock-row")]
    #[tokio::test]
    async fn lazy_pool_creates_table_on_first_use() {
        let server = start().await;
        let config = SqlxPgConfig {
            connect: connect(&server),
            pool: PoolConfig {
                connect_lazy: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let lazy = SqlxPgPool::from_config(&config, &app("lazy"))
            .await
            .unwrap();
        assert!(!table_exists(&lazy, "lazy_lock").await);
        let guard = lazy.try_lock(lock_key!(TestLockKey::Job, 1)).await.unwrap();
        assert!(guard.is_some());
        assert!(table_exists(&lazy, "lazy_lock").await);
    }

    /// 已有同名的表但列对不上：不动它，拒绝启动
    #[cfg(feature = "lock-row")]
    #[tokio::test]
    async fn mismatched_table_refuses_to_start() {
        let server = start().await;
        let other = pool(&server).await;
        sqlx::raw_sql("CREATE TABLE bad_lock (key integer PRIMARY KEY)")
            .execute(&*other)
            .await
            .unwrap();
        let config = SqlxPgConfig {
            connect: connect(&server),
            ..Default::default()
        };
        let e = SqlxPgPool::from_config(&config, &app("bad"))
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LockTableSchemaMismatch), "{e:#}");
    }
}
