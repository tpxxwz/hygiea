//! toasty 的 PostgreSQL 连接池上的锁和动态配置（实现在 `lock/toasty/`、`live_config/toasty/`），container 层，要 Docker：
//! ```sh
//! cargo test -p hygiea-db --features toasty,postgres,lock,lock-row,live-config --test toasty_pg -- --ignored ::container::
//! cargo test -p hygiea-db --features toasty,postgres,lock,live-config --test toasty_pg -- --ignored ::container::
//! ```

#![cfg(all(
    feature = "toasty",
    feature = "postgres",
    feature = "lock",
    feature = "live-config"
))]

use std::future::Future;
use std::time::Duration;

use hygiea_core::lock::{DistributedLock, hy_lock_key, lock_key};
use hygiea_test::container;
use serde::{Deserialize, Serialize};

#[derive(hy_lock_key)]
#[lock_internal_module_prefix = "100"]
enum TestLockKey {
    #[lock_key(code = "003", tpl = "toasty:{{ id }}")]
    Job,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Job {
    enabled: bool,
    levels: Vec<i32>,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            enabled: false,
            levels: vec![1, 2],
        }
    }
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

#[container]
mod postgres {
    use hygiea_core::app::{ConfigResource, RegistryConfig};
    use hygiea_db::{LiveConfigOptions, PgConnectConfig, ToastyPgConfig, ToastyPgPool};
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

    /// 应用名 `hy`：锁表 hy_lock，配置表 hy_config 那三张
    fn app() -> RegistryConfig {
        RegistryConfig {
            app_name: "hy".to_string(),
            ..Default::default()
        }
    }

    /// 一个连接池模拟一个实例；动态配置只靠轮询，间隔 1 秒
    async fn pool(server: &RunningContainer) -> ToastyPgPool {
        let config = ToastyPgConfig {
            connect: PgConnectConfig {
                host: server.host().to_string(),
                port: server.port(PORT),
                password: PASSWORD.to_string(),
                ..Default::default()
            },
            live_config: LiveConfigOptions {
                enabled: true,
                poll_interval_secs: Some(1),
                ..Default::default()
            },
        };
        ToastyPgPool::from_config(&config, &app()).await.unwrap()
    }

    async fn exec(pool: &ToastyPgPool, sql: &str) {
        let mut db = (**pool).clone();
        toasty::sql::statement(sql).exec(&mut db).await.unwrap();
    }

    /// 整数 key：别的实例拿不到，release 后能拿到；阻塞版等满 timeout 返回 None，别人释放了就拿到
    #[tokio::test]
    async fn advisory_lock_is_exclusive() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        let guard = a.try_lock_i64(1).await.unwrap().unwrap();
        assert_eq!(guard.key(), "advisory 1");
        assert!(b.try_lock_i64(1).await.unwrap().is_none());
        assert!(b.try_lock_i64(2).await.unwrap().is_some());

        let start = tokio::time::Instant::now();
        assert!(
            b.lock_i64(1, Duration::from_millis(300))
                .await
                .unwrap()
                .is_none()
        );
        assert!(start.elapsed() >= Duration::from_millis(300));

        let waiter = tokio::spawn(async move { b.lock_i64(1, Duration::from_secs(10)).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        guard.release().await.unwrap();
        assert!(waiter.await.unwrap().unwrap().is_some());
    }

    /// 字符串 key：开了 lock-row 走锁表，没开哈希成 advisory lock，两种都互斥
    #[tokio::test]
    async fn string_key_is_exclusive() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        let guard = a
            .try_lock(lock_key!(TestLockKey::Job, 1))
            .await
            .unwrap()
            .unwrap();
        assert!(
            b.try_lock(lock_key!(TestLockKey::Job, 1))
                .await
                .unwrap()
                .is_none()
        );
        guard.release().await.unwrap();
        assert!(
            b.lock(lock_key!(TestLockKey::Job, 1), Duration::from_secs(5))
                .await
                .unwrap()
                .is_some()
        );
    }

    /// 第一次 bind 建表、补默认值；表里改了值，轮询读到
    #[tokio::test]
    async fn live_config_polls_changes() {
        let server = start().await;
        let first = pool(&server).await;
        let mut sync = first.live_config("sync", Job::default()).await.unwrap();
        assert_eq!(*sync.get(), Job::default());

        exec(
            &first,
            "UPDATE hy_config SET value = '[3, 4]' WHERE namespace = 'sync' AND key = 'levels'",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), sync.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sync.get().levels, vec![3, 4]);

        // 另一个实例绑同一个 namespace：表已经在了，直接读到改过的值
        let other = pool(&server).await;
        let view = other.live_config("sync", Job::default()).await.unwrap();
        eventually(5, || async { view.get().levels == vec![3, 4] }).await;
    }
}
