//! `SqlxMySqlPool`（`src/sqlx/pool/mysql.rs`，feature `sqlx` + `mysql`）的集成测试，feature 全开（`lock`、`live-config`），container 层：
//! 用代码启动官方 mysql 容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）
//! ```sh
//! cargo test -p hygiea-db --features sqlx,mysql,lock,live-config --test sqlx_mysql -- --ignored ::container::
//! ```

#![cfg(all(
    feature = "sqlx",
    feature = "mysql",
    feature = "lock",
    feature = "live-config"
))]

mod common;

use std::time::Duration;

use common::eventually;
use hygiea_core::app::{ConfigResource, RegistryConfig};
use hygiea_core::lock::{DistributedLock, hy_lock_key, hy_lock_type, lock_key};
use hygiea_db::{MySqlConnectConfig, SqlxMySqlConfig, SqlxMySqlPool};
use hygiea_test::container;
use hygiea_test::container::{ContainerSpec, RunningContainer};

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

const PASSWORD: &str = "hygiea-test";

const DATABASE: &str = "hy";

const PORT: u16 = 3306;

/// 镜像固定版本（8.x 的 LTS）。启动时先起临时实例（`port: 0`）做初始化再重启，就绪日志出现两次，第二次才是正式实例
async fn start() -> RunningContainer {
    ContainerSpec::new("mysql", "8.4")
        .env("MYSQL_ROOT_PASSWORD", PASSWORD)
        .env("MYSQL_DATABASE", DATABASE)
        .port(PORT)
        .wait_log_times("mysqld: ready for connections", 2)
        .startup_timeout(Duration::from_secs(180))
        .start()
        .await
}

fn connect(server: &RunningContainer) -> MySqlConnectConfig {
    MySqlConnectConfig {
        host: server.host().to_string(),
        port: server.port(PORT),
        password: PASSWORD.to_string(),
        database: DATABASE.to_string(),
        ..Default::default()
    }
}

/// 应用名 `hy`：动态配置的表是 hy_config 那三张
fn app() -> RegistryConfig {
    RegistryConfig {
        app_name: "hy".to_string(),
        ..Default::default()
    }
}

fn config(server: &RunningContainer) -> SqlxMySqlConfig {
    SqlxMySqlConfig {
        connect: connect(server),
        ..Default::default()
    }
}

/// 一个连接池模拟一个实例
async fn pool(server: &RunningContainer) -> SqlxMySqlPool {
    SqlxMySqlPool::from_config(&config(server), &app())
        .await
        .unwrap()
}

async fn exec(pool: &SqlxMySqlPool, sql: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&**pool)
        .await
        .unwrap();
}

/// 连接池本身：连接、经 Registry 启动、HyUtcDateTime、事务
#[container]
mod pool {
    use hygiea_core::app::{Registry, Resources};
    use hygiea_core::datetime::{HygieaDateTimeExt, UtcDateTime};
    use hygiea_core::{ResultExt, err};
    use hygiea_db::{HyUtcDateTime, RepoErr, SqlxMySqlComponent};

    use super::*;

    /// 启动 registry，把 `Resources` 交给 `check`，拿回它的结果。
    /// 启动成功后 `run` 会一直等退出信号，所以放到后台任务里，拿到结果就 abort
    async fn with_resources<T: Send + 'static>(
        registry: Registry,
        check: impl FnOnce(Resources) -> T + Send + 'static,
    ) -> T {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = registry
                .on_ready(move |resources| async move {
                    let _ = tx.send(check(resources));
                    Ok(())
                })
                .run()
                .await;
        });
        let result = rx.await.unwrap();
        task.abort();
        result
    }

    /// 经 Registry 启动，按组件名取到能用的连接池；HyUtcDateTime 写进 TIMESTAMP 再读回；
    /// 事务闭包返回 Err 时回滚；关闭后再查询报 PoolClosed
    #[tokio::test]
    async fn registry_datetime_transaction_close() {
        let server = start().await;
        let registry = Registry::new().add_named::<SqlxMySqlComponent>("primary", config(&server));
        let pool = with_resources(registry, |res| res.get_named::<SqlxMySqlPool>("primary"))
            .await
            .unwrap();
        let value: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(value, 1);

        exec(
            &pool,
            "CREATE TABLE record (id BIGINT PRIMARY KEY, create_at TIMESTAMP(6) NOT NULL)",
        )
        .await;
        let at = HyUtcDateTime(UtcDateTime::parse_ext_rfc3339("2026-10-05T12:57:24.719Z").unwrap());
        sqlx::query("INSERT INTO record (id, create_at) VALUES (?, ?)")
            .bind(1_i64)
            .bind(at)
            .execute(&*pool)
            .await
            .unwrap();
        let read: HyUtcDateTime = sqlx::query_scalar("SELECT create_at FROM record WHERE id = 1")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(read, at);

        // 第二条主键冲突：整个事务回滚，第一条也不留
        let e = hygiea_db::sqlx::transaction(&*pool, "save record", async |tx| {
            for id in [2_i64, 1] {
                sqlx::query("INSERT INTO record (id, create_at) VALUES (?, NOW())")
                    .bind(id)
                    .execute(&mut **tx)
                    .await
                    .wrap_err(|| err!(RepoErr::InsertFailed, "insert record"))?;
            }
            Ok(())
        })
        .await
        .unwrap_err();
        assert!(e.is(RepoErr::InsertFailed), "{e:#}");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM record")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "回滚了");

        pool.close().await.unwrap();
        let err = sqlx::query("SELECT 1").fetch_one(&*pool).await.unwrap_err();
        assert!(matches!(err, sqlx::Error::PoolClosed), "{err:?}");
    }
}

/// 分布式锁：`GET_LOCK`
#[container]
mod lock {
    use hygiea_core::lock::{LockErr, LockGuard};

    use super::*;

    async fn is_free(pool: &SqlxMySqlPool, name: &str) -> bool {
        let free: Option<i64> = sqlx::query_scalar("SELECT IS_FREE_LOCK(?)")
            .bind(name)
            .fetch_one(&**pool)
            .await
            .unwrap();
        free == Some(1)
    }

    /// 整数 key：别的实例拿不到，自己再拿也拿不到（每次一条新连接，不可重入），release 后别人能拿到；
    /// 锁类型 + i32 和 i64 是两个 key 空间；guard drop 时在后台释放
    #[tokio::test]
    async fn integer_keys_are_exclusive() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        let guard = a.try_lock_i64(1).await.unwrap().unwrap();
        assert_eq!(guard.key(), "i64:1");
        assert!(b.try_lock_i64(1).await.unwrap().is_none());
        assert!(a.try_lock_i64(1).await.unwrap().is_none());
        assert!(b.try_lock_i64(2).await.unwrap().is_some());
        guard.release().await.unwrap();
        assert!(is_free(&a, "i64:1").await);
        assert!(b.try_lock_i64(1).await.unwrap().is_some());

        let pair = a
            .try_lock_i32(TestLockType::Job, 42)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pair.key(), "i32:999100001:42");
        assert!(
            b.try_lock_i32(TestLockType::Job, 42)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            b.try_lock_i64(42).await.unwrap().is_some(),
            "和 i64 是两个空间"
        );

        drop(pair);
        eventually(5, || async {
            b.try_lock_i32(TestLockType::Job, 42)
                .await
                .unwrap()
                .is_some()
        })
        .await;
    }

    /// 字符串 key 原样做锁名；阻塞版等到超时返回 None（整数秒），别人释放了马上拿到；
    /// 超过 64 个字符报 InvalidKey、带零头的超时报 InvalidTimeout，都不发 SQL
    #[tokio::test]
    async fn string_key_and_blocking() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        let key = || lock_key!(TestLockKey::Job, 1);
        let guard = a.try_lock(key()).await.unwrap().unwrap();
        assert_eq!(guard.key(), "999100002:job:1");
        assert!(b.try_lock(key()).await.unwrap().is_none());
        assert!(
            b.try_lock(lock_key!(TestLockKey::Job, 2))
                .await
                .unwrap()
                .is_some()
        );

        let start = tokio::time::Instant::now();
        assert!(
            b.lock(key(), Duration::from_secs(1))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            start.elapsed() >= Duration::from_millis(900),
            "{:?}",
            start.elapsed()
        );
        assert!(b.lock(key(), Duration::ZERO).await.unwrap().is_none());

        let waiter = {
            let b = b.clone();
            tokio::spawn(async move { b.lock(key(), Duration::from_secs(10)).await })
        };
        tokio::time::sleep(Duration::from_millis(500)).await;
        let released = tokio::time::Instant::now();
        guard.release().await.unwrap();
        let got = waiter.await.unwrap().unwrap().unwrap();
        assert!(
            released.elapsed() < Duration::from_secs(1),
            "释放了马上拿到"
        );
        got.release().await.unwrap();

        let long = lock_key!(TestLockKey::Job, "x".repeat(60));
        let e = a.try_lock(long).await.unwrap_err();
        assert!(e.is(LockErr::InvalidKey), "{e:#}");
        let e = b
            .lock_i64(1, Duration::from_millis(1500))
            .await
            .unwrap_err();
        assert!(e.is(LockErr::InvalidTimeout), "{e:#}");
    }

    /// DBA 在全局设了很短的超时：持锁空闲超过 wait_timeout 也不被断开；等锁不被 max_execution_time 打断；
    /// 释放后连接的 wait_timeout 恢复成全局值再还回池
    #[tokio::test]
    async fn dba_timeouts_do_not_cut_locks() {
        let server = start().await;
        let root = pool(&server).await;
        exec(
            &root,
            "SET GLOBAL wait_timeout = 2; SET GLOBAL max_execution_time = 300",
        )
        .await;
        // 全局值只对之后建的连接生效
        let mut one = config(&server);
        one.pool.max_connections = Some(1);
        let a = SqlxMySqlPool::from_config(&one, &app()).await.unwrap();
        let b = pool(&server).await;

        let guard = a.try_lock_i64(1).await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !is_free(&root, "i64:1").await,
            "持锁的连接空闲 4 秒没被断开"
        );

        let start = tokio::time::Instant::now();
        assert!(
            b.lock_i64(1, Duration::from_secs(2))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            start.elapsed() >= Duration::from_millis(1900),
            "等满了，没被打断"
        );

        guard.release().await.unwrap();
        let restored: u64 = sqlx::query_scalar("SELECT @@session.wait_timeout")
            .fetch_one(&*a)
            .await
            .unwrap();
        assert_eq!(restored, 2, "还回池的连接恢复成全局值");
    }

    /// 等锁时被调用方取消（外层 timeout）：连接被关掉，等到的锁不会挂在池里的连接上；池里只有一条连接也能马上接着用
    #[tokio::test]
    async fn cancelled_wait_closes_connection() {
        let server = start().await;
        let a = pool(&server).await;
        let mut one = config(&server);
        one.pool.max_connections = Some(1);
        one.pool.acquire_timeout_secs = Some(2);
        let b = SqlxMySqlPool::from_config(&one, &app()).await.unwrap();
        let guard = a.try_lock_i64(1).await.unwrap().unwrap();

        let waiting = tokio::time::timeout(
            Duration::from_millis(300),
            b.lock_i64(1, Duration::from_secs(30)),
        )
        .await;
        assert!(waiting.is_err(), "还在等锁时被取消");
        assert!(b.try_lock_i64(2).await.unwrap().is_some());

        guard.release().await.unwrap();
        eventually(5, || async { is_free(&a, "i64:1").await }).await;
    }

    /// `[db.lock]` 写了 max_connections：锁用单独的池，持锁不占连接池的连接
    #[tokio::test]
    async fn own_pool_does_not_use_main_pool() {
        let server = start().await;
        let mut own = config(&server);
        own.pool.max_connections = Some(1);
        own.pool.acquire_timeout_secs = Some(2);
        own.lock.max_connections = Some(2);
        let a = SqlxMySqlPool::from_config(&own, &app()).await.unwrap();
        let held: Vec<LockGuard> = vec![
            a.try_lock_i64(1).await.unwrap().unwrap(),
            a.try_lock_i64(2).await.unwrap().unwrap(),
        ];
        let value: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&*a).await.unwrap();
        assert_eq!(value, 1, "连接池的唯一一条连接没被锁占着");
        for guard in held {
            guard.release().await.unwrap();
        }
    }
}

/// 动态配置：只靠轮询
#[container]
mod live_config {
    use hygiea_db::{MySqlLiveConfigConfig, RepoErr};
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Job {
        enabled: bool,
        levels: Vec<i32>,
        start_time: String,
    }

    impl Default for Job {
        fn default() -> Self {
            Self {
                enabled: false,
                levels: vec![1, 2],
                start_time: "01:00".to_string(),
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Cache {
        ttl_secs: u64,
    }

    impl Default for Cache {
        fn default() -> Self {
            Self { ttl_secs: 60 }
        }
    }

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    struct All {
        sync: Job,
        cache: Cache,
    }

    /// 轮询间隔 1 秒
    fn live_config() -> MySqlLiveConfigConfig {
        MySqlLiveConfigConfig {
            enabled: true,
            poll_interval_secs: Some(1),
            ..Default::default()
        }
    }

    async fn live_pool(server: &RunningContainer) -> SqlxMySqlPool {
        let config = SqlxMySqlConfig {
            connect: connect(server),
            live_config: live_config(),
            ..Default::default()
        };
        SqlxMySqlPool::from_config(&config, &app()).await.unwrap()
    }

    /// 第一次 bind 建表、补默认值（类型原样存成 JSON，已有的 key 不动、不记历史）；改值、删 key（退回默认值）
    /// 靠轮询拿到；history 记了账号和 I / U / D
    #[tokio::test]
    async fn seeds_and_polls_changes() {
        let server = start().await;
        let source = live_pool(&server).await;
        let all = source.live_config_all(All::default()).await.unwrap();
        assert_eq!(*all.get(), All::default());

        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT namespace, `key`, CAST(value AS CHAR) FROM hy_config ORDER BY namespace, `key`",
        )
        .fetch_all(&*source)
        .await
        .unwrap();
        let rows: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|(n, k, v)| (n.as_str(), k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("cache", "ttl_secs", "60"),
                ("sync", "enabled", "false"),
                ("sync", "levels", "[1, 2]"),
                ("sync", "start_time", "\"01:00\""),
            ]
        );

        exec(
            &source,
            "BEGIN; \
             UPDATE hy_config SET value = 'true' WHERE namespace = 'sync' AND `key` = 'enabled'; \
             UPDATE hy_config SET value = '5' WHERE namespace = 'cache' AND `key` = 'ttl_secs'; \
             DELETE FROM hy_config WHERE namespace = 'sync' AND `key` = 'levels'; \
             COMMIT;",
        )
        .await;
        eventually(5, || async {
            let all = all.get();
            all.sync.enabled && all.cache.ttl_secs == 5
        })
        .await;
        assert_eq!(all.get().sync.levels, vec![1, 2], "删掉的 key 退回默认值");

        // 再绑一次同一个 namespace：已有的 key 不动、不记历史，删掉的 key 补回默认值
        let mut sync = source.live_config("sync", Job::default()).await.unwrap();
        assert!(sync.get().enabled);
        let ops: Vec<String> = sqlx::query_scalar(
            "SELECT op FROM hy_config_history WHERE namespace = 'sync' ORDER BY id",
        )
        .fetch_all(&*source)
        .await
        .unwrap();
        assert_eq!(ops, vec!["I", "I", "I", "U", "D", "I"]);
        let by: String =
            sqlx::query_scalar("SELECT changed_by FROM hy_config_history ORDER BY id DESC LIMIT 1")
                .fetch_one(&*source)
                .await
                .unwrap();
        assert!(by.starts_with("root@"), "{by}");

        exec(
            &source,
            "UPDATE hy_config SET value = '\"02:00\"' WHERE namespace = 'sync' AND `key` = 'start_time'",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), sync.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sync.get().start_time, "02:00");
    }

    /// 几个实例同时第一次 bind（同时建表），都成功；表名前缀可配
    #[tokio::test]
    async fn concurrent_startup_and_prefix() {
        let server = start().await;
        let mut binds = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let pool = live_pool(&server).await;
            binds.spawn(async move {
                pool.live_config("cache", Cache::default())
                    .await
                    .map(|_| ())
            });
        }
        while let Some(result) = binds.join_next().await {
            result.unwrap().unwrap();
        }

        let config = SqlxMySqlConfig {
            connect: connect(&server),
            live_config: MySqlLiveConfigConfig {
                table_prefix: Some("custom".to_string()),
                ..live_config()
            },
            ..Default::default()
        };
        let custom = SqlxMySqlPool::from_config(&config, &app()).await.unwrap();
        custom.live_config("cache", Cache::default()).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom_config")
            .fetch_one(&*custom)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    /// 已有同名的表但结构对不上：不动它，拒绝启动；历史表保留行数在 MySQL 上也生效
    #[tokio::test]
    async fn mismatch_refuses_and_history_trims() {
        let server = start().await;
        let other = pool(&server).await;
        exec(
            &other,
            "CREATE TABLE bad_config (namespace INT PRIMARY KEY)",
        )
        .await;
        let config = SqlxMySqlConfig {
            connect: connect(&server),
            live_config: MySqlLiveConfigConfig {
                table_prefix: Some("bad".to_string()),
                ..live_config()
            },
            ..Default::default()
        };
        let bad = SqlxMySqlPool::from_config(&config, &app()).await.unwrap();
        let e = bad
            .live_config("cache", Cache::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigSchemaMismatch), "{e:#}");

        let source = live_pool(&server).await;
        source.live_config("sync", Job::default()).await.unwrap();
        exec(
            &source,
            "UPDATE hy_config SET value = '[3]' WHERE `key` = 'levels'",
        )
        .await;
        let mut trimmed = SqlxMySqlConfig {
            connect: connect(&server),
            live_config: live_config(),
            ..Default::default()
        };
        trimmed.live_config.history_keep_rows = Some(2);
        trimmed.live_config.history_trim_interval_mins = Some(60);
        let restarted = SqlxMySqlPool::from_config(&trimmed, &app()).await.unwrap();
        restarted.live_config("sync", Job::default()).await.unwrap();
        eventually(5, || async {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hy_config_history")
                .fetch_one(&*source)
                .await
                .unwrap();
            count == 2
        })
        .await;
    }

    /// DBA 在全局设了很短的 max_execution_time、wait_timeout：动态配置的小池不受影响，空闲后照样读到新值
    #[tokio::test]
    async fn dba_timeouts_do_not_break_polling() {
        let server = start().await;
        let root = pool(&server).await;
        exec(
            &root,
            "SET GLOBAL wait_timeout = 2; SET GLOBAL max_execution_time = 1",
        )
        .await;
        let source = live_pool(&server).await;
        let mut cache = source.live_config("cache", Cache::default()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await;
        exec(
            &root,
            "UPDATE hy_config SET value = '9' WHERE namespace = 'cache'",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), cache.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cache.get().ttl_secs, 9);
    }
}
