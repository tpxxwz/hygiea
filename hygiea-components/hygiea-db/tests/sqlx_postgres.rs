//! `SqlxPgPool`（`src/sqlx/pool/postgres.rs`，feature `sqlx` + `postgres`）的集成测试，feature 全开（`lock-row`、`live-config`），container 层：
//! 用代码启动官方 postgres 容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）
//! ```sh
//! cargo test -p hygiea-db --features sqlx,postgres,lock-row,live-config --test sqlx_postgres -- --ignored ::container::
//! ```
//! 不开 `lock-row` 时字符串 key 返回 `LockErr::Unsupported`，这条路径不开 `lock-row` 跑：
//! ```sh
//! cargo test -p hygiea-db --features sqlx,postgres,lock --test sqlx_postgres -- --ignored ::container::
//! ```

#![cfg(all(feature = "sqlx", feature = "postgres", feature = "lock"))]

mod common;

use std::time::Duration;

use common::eventually;
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

/// 连接池本身：连接、search_path、经 Registry 启动和停止、HyUtcDateTime、事务、分页
#[container]
mod pool {
    use hygiea_core::app::{Registry, Resources};
    use hygiea_core::datetime::{HygieaDateTimeExt, UtcDateTime};
    use hygiea_db::HyUtcDateTime;
    use hygiea_db::{
        DbResources, PgConnectConfig, PoolConfig, SqlxPgComponent, SqlxPgConfig, SqlxPgPool,
    };

    use super::common::open_pool;
    use hygiea_test::container::{ContainerSpec, RunningContainer};

    const USER: &str = "postgres";

    const PASSWORD: &str = "hygiea-test";

    const DATABASE: &str = "postgres";

    const PORT: u16 = 5432;

    /// 测试用的非 public schema，和 [`create_schema`] 里建的一致
    const SCHEMA: &str = "hygiea_test";

    /// 镜像固定版本。启动时先起临时实例跑初始化再重启，就绪日志会出现两次，第二次才是正式实例
    async fn start() -> RunningContainer {
        ContainerSpec::new("postgres", "18")
            .env("POSTGRES_USER", USER)
            .env("POSTGRES_PASSWORD", PASSWORD)
            .env("POSTGRES_DB", DATABASE)
            .port(PORT)
            .wait_log_times("database system is ready to accept connections", 2)
            .start()
            .await
    }

    fn connect(server: &RunningContainer) -> PgConnectConfig {
        PgConnectConfig {
            host: server.host().to_string(),
            port: server.port(PORT),
            username: USER.to_string(),
            password: PASSWORD.to_string(),
            database: DATABASE.to_string(),
            ..Default::default()
        }
    }

    fn sqlx_config(server: &RunningContainer) -> SqlxPgConfig {
        SqlxPgConfig {
            connect: connect(server),
            ..Default::default()
        }
    }

    /// 用默认配置连上去建好 [`SCHEMA`]，search_path 指向不存在的 schema 时 current_schema() 是 NULL
    async fn create_schema(server: &RunningContainer) {
        let db = DbResources::from_config(&sqlx_config(server), &Default::default())
            .await
            .unwrap();
        sqlx::query("CREATE SCHEMA hygiea_test")
            .execute(&**db.pool.as_ref().unwrap())
            .await
            .unwrap();
        db.close().await.unwrap();
    }

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

    #[tokio::test]
    async fn sqlx_connect_success() {
        let server = start().await;
        let pool = open_pool(&sqlx_config(&server), &Default::default()).await;

        let value: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(value, 1);
    }

    #[tokio::test]
    async fn sqlx_schema_search_path_takes_effect() {
        let server = start().await;
        create_schema(&server).await;

        let mut config = sqlx_config(&server);
        config.connect.schema_search_path = SCHEMA.to_string();
        let pool = open_pool(&config, &Default::default()).await;

        let schema: Option<String> = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(schema.as_deref(), Some(SCHEMA));
    }

    #[tokio::test]
    async fn sqlx_stop_closes_pool() {
        let server = start().await;
        let mut config = sqlx_config(&server);
        config.pool = Some(PoolConfig {
            max_connections: Some(2),
            ..Default::default()
        });
        let db = DbResources::from_config(&config, &Default::default())
            .await
            .unwrap();
        let pool = db.pool.clone().unwrap();

        sqlx::query("SELECT 1").fetch_one(&*pool).await.unwrap();

        db.close().await.unwrap();

        // 关闭后再发查询应该失败
        let err = sqlx::query("SELECT 1").fetch_one(&*pool).await.unwrap_err();
        assert!(matches!(err, sqlx::Error::PoolClosed), "{err:?}");
    }

    /// SqlxPgComponent 经 Registry 启动，按组件名取到能用的连接池
    #[tokio::test]
    async fn sqlx_component_startup_via_registry() {
        let server = start().await;
        let registry =
            Registry::new().add_named::<SqlxPgComponent>("primary", sqlx_config(&server));
        let pool = with_resources(registry, |res| res.get_named::<SqlxPgPool>("primary"))
            .await
            .unwrap();

        let value: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(value, 1);
    }

    /// `uses = 7` 经 Registry 启动：业务池、动态配置按组件名，锁匿名，都能用。再加一个只给锁用（`uses = 4`）的组件时
    /// 两个组件都提供锁，启动前就报 DuplicateProvider
    #[cfg(feature = "live-config")]
    #[tokio::test]
    async fn uses_puts_resources_via_registry() {
        use hygiea_core::app::AppErr;
        use hygiea_core::lock::DistributedLock;
        use hygiea_db::{DbLiveConfig, DbLockClient, DbUses};

        #[derive(Default, serde::Serialize, serde::Deserialize)]
        struct Cache {
            ttl_secs: u64,
        }

        let server = start().await;
        let all = SqlxPgConfig {
            uses: DbUses::try_from(7).unwrap(),
            ..sqlx_config(&server)
        };
        let registry = Registry::new().add_named::<SqlxPgComponent>("main", all.clone());
        let (pool, live, lock_client) = with_resources(registry, |res| {
            (
                res.get_named::<SqlxPgPool>("main").unwrap(),
                res.get_named::<DbLiveConfig>("main").unwrap(),
                res.get::<DbLockClient>().unwrap(),
            )
        })
        .await;
        let value: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(value, 1);
        assert_eq!(
            live.bind("cache", Cache::default())
                .await
                .unwrap()
                .get()
                .ttl_secs,
            0
        );
        assert!(lock_client.try_lock_i64(1).await.unwrap().is_some());

        let lock_only = SqlxPgConfig {
            uses: DbUses::try_from(4).unwrap(),
            ..sqlx_config(&server)
        };
        let e = Registry::new()
            .add_named::<SqlxPgComponent>("main", all)
            .add_named::<SqlxPgComponent>("coord", lock_only)
            .run()
            .await
            .0
            .unwrap_err();
        assert!(e.is(AppErr::DuplicateProvider), "{e:#}");
    }

    // ---- HyUtcDateTime ------------------------------------------------------

    /// 存 `HyUtcDateTime` 的表：一个必填列、一个可空列，都是 `timestamptz`
    const CREATE_RECORD: &str = "CREATE TABLE record (\
        id BIGINT PRIMARY KEY, \
        create_at TIMESTAMPTZ NOT NULL, \
        delete_at TIMESTAMPTZ)";

    fn utc(s: &str) -> UtcDateTime {
        UtcDateTime::parse_ext_rfc3339(s).unwrap()
    }

    /// sqlx 往 timestamptz 列写入再读回，可空列写 NULL 读回 None；
    /// 库里按 +08:00 写的值读出来换算成 UTC
    #[tokio::test]
    async fn sqlx_hy_utc_datetime_round_trip() {
        let server = start().await;
        let pool = open_pool(&sqlx_config(&server), &Default::default()).await;
        sqlx::query(CREATE_RECORD).execute(&*pool).await.unwrap();

        let at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        sqlx::query(
            "INSERT INTO record (id, create_at, delete_at) VALUES ($1, $2, $3), ($4, $5, $6)",
        )
        .bind(1_i64)
        .bind(at)
        .bind(Some(at))
        .bind(2_i64)
        .bind(at)
        .bind(None::<HyUtcDateTime>)
        .execute(&*pool)
        .await
        .unwrap();

        let rows: Vec<(i64, HyUtcDateTime, Option<HyUtcDateTime>)> =
            sqlx::query_as("SELECT id, create_at, delete_at FROM record ORDER BY id")
                .fetch_all(&*pool)
                .await
                .unwrap();
        assert_eq!(rows, vec![(1, at, Some(at)), (2, at, None)]);

        let shanghai: HyUtcDateTime =
            sqlx::query_scalar("SELECT '2026-10-05 20:57:24.719+08'::timestamptz")
                .fetch_one(&*pool)
                .await
                .unwrap();
        assert_eq!(shanghai, at);
    }

    /// sqlx::transaction：闭包 Ok 就提交；Err 就回滚，闭包的错误原样返回
    #[tokio::test]
    async fn sqlx_transaction_commits_or_rolls_back() {
        use hygiea_core::{ResultExt, err};
        use hygiea_db::RepoErr;

        let server = start().await;
        let pool = open_pool(&sqlx_config(&server), &Default::default()).await;
        sqlx::query("CREATE TABLE item (id INT PRIMARY KEY)")
            .execute(&*pool)
            .await
            .unwrap();
        let insert = async |tx: &mut sqlx::Transaction<'static, sqlx::Postgres>, id: i32| {
            sqlx::query("INSERT INTO item VALUES ($1)")
                .bind(id)
                .execute(&mut **tx)
                .await
                .wrap_err(|| err!(RepoErr::InsertFailed, "insert item"))
        };

        hygiea_db::sqlx::transaction(&pool, "save item", async |tx| {
            insert(tx, 1).await?;
            insert(tx, 2).await?;
            Ok(())
        })
        .await
        .unwrap();

        // 第二条主键冲突：整个事务回滚，第一条也不留
        let e = hygiea_db::sqlx::transaction(&pool, "save item", async |tx| {
            insert(tx, 3).await?;
            insert(tx, 1).await?;
            Ok(())
        })
        .await
        .unwrap_err();
        assert!(e.is(RepoErr::InsertFailed), "{e:#}");

        let ids: Vec<i32> = sqlx::query_scalar("SELECT id FROM item ORDER BY id")
            .fetch_all(&*pool)
            .await
            .unwrap();
        assert_eq!(ids, vec![1, 2]);
    }

    /// sqlx 自己写 LIMIT / OFFSET，绑 PageRequest 的 limit()、offset()，再用 Page::new 拼结果
    #[tokio::test]
    async fn sqlx_page_request_binds_limit_offset() {
        use hygiea_db::{Page, PageRequest};

        let server = start().await;
        let pool = open_pool(&sqlx_config(&server), &Default::default()).await;
        sqlx::query(CREATE_RECORD).execute(&*pool).await.unwrap();
        sqlx::query(
            "INSERT INTO record (id, create_at) SELECT g, now() FROM generate_series(1, 5) AS g",
        )
        .execute(&*pool)
        .await
        .unwrap();

        let request = PageRequest::new(1, 2).unwrap();
        let items: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM record ORDER BY id LIMIT $1 OFFSET $2")
                .bind(request.limit())
                .bind(request.offset())
                .fetch_all(&*pool)
                .await
                .unwrap();
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM record")
            .fetch_one(&*pool)
            .await
            .unwrap();
        let page = Page::new(items, request, total.unsigned_abs());
        assert_eq!(page, Page::new(vec![3, 4], request, 5));
        assert_eq!(page.total_pages, 3);
    }
}

/// 分布式锁（feature `lock`；开了 `lock-row` 测锁表，不开时测字符串 key 不支持）
#[container]
mod lock {
    use hygiea_core::app::RegistryConfig;
    use hygiea_db::{PgConnectConfig, PoolConfig, SqlxPgConfig};
    use hygiea_test::container::{ContainerSpec, RunningContainer};

    use super::common::{LockInst, uses};
    use super::*;

    type Inst = LockInst<SqlxPgConfig>;

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

    /// 一个实例：业务池加锁（`uses = 5`），锁和业务池共用连接
    async fn pool(server: &RunningContainer) -> Inst {
        let config = SqlxPgConfig {
            uses: uses(5),
            connect: connect(server),
            ..Default::default()
        };
        Inst::open(&config, &app("hy")).await
    }

    #[cfg(feature = "lock-row")]
    async fn table_exists(pool: &hygiea_db::SqlxPgPool, table: &str) -> bool {
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
        .fetch_one(&**b.pool())
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

    /// DBA 在用户上设了很短的四个超时：锁事务里 SET LOCAL 成 0，持锁比它们都久也不被断开，
    /// 阻塞版等满自己的 timeout（不被 statement_timeout 提前打断）；同一个用户的普通查询照旧受限
    #[tokio::test]
    async fn dba_timeouts_do_not_cut_locks() {
        let server = start().await;
        // 先用 postgres 连一次：开了 lock-row 时由它建锁表
        let admin = pool(&server).await;
        sqlx::raw_sql(
            "CREATE ROLE strict LOGIN SUPERUSER PASSWORD 'strict'; \
             ALTER ROLE strict SET lock_timeout = '100ms'; \
             ALTER ROLE strict SET statement_timeout = '100ms'; \
             ALTER ROLE strict SET idle_in_transaction_session_timeout = '200ms'; \
             ALTER ROLE strict SET transaction_timeout = '300ms'",
        )
        .execute(&**admin.pool())
        .await
        .unwrap();
        let strict = || async {
            let config = SqlxPgConfig {
                uses: uses(5),
                connect: PgConnectConfig {
                    username: "strict".to_string(),
                    password: "strict".to_string(),
                    ..connect(&server)
                },
                ..Default::default()
            };
            Inst::open(&config, &app("hy")).await
        };
        let (a, b) = (strict().await, strict().await);

        let slow = sqlx::query("SELECT pg_sleep(0.5)")
            .execute(&**a.pool())
            .await;
        assert!(slow.is_err(), "普通查询仍受 statement_timeout 限制");

        let guard = a.try_lock_i64(1).await.unwrap().unwrap();
        #[cfg(feature = "lock-row")]
        let key = lock_key!(TestLockKey::Job, 1);
        #[cfg(feature = "lock-row")]
        let row = a.try_lock(key.clone()).await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(b.try_lock_i64(1).await.unwrap().is_none(), "持锁没被断开");
        #[cfg(feature = "lock-row")]
        assert!(b.try_lock(key.clone()).await.unwrap().is_none());

        let start = tokio::time::Instant::now();
        assert!(
            b.lock_i64(1, Duration::from_millis(500))
                .await
                .unwrap()
                .is_none()
        );
        assert!(start.elapsed() >= Duration::from_millis(500));
        #[cfg(feature = "lock-row")]
        assert!(
            b.lock(key, Duration::from_millis(500))
                .await
                .unwrap()
                .is_none()
        );

        guard.release().await.unwrap();
        #[cfg(feature = "lock-row")]
        row.release().await.unwrap();
    }

    /// 等锁时被调用方取消（外层 timeout）：连接被关掉、不占着池，池里只有一条连接也能马上接着用
    #[tokio::test]
    async fn cancelled_wait_frees_pool_connection() {
        let server = start().await;
        let a = pool(&server).await;
        let config = SqlxPgConfig {
            uses: uses(5),
            connect: connect(&server),
            pool: Some(PoolConfig {
                max_connections: Some(1),
                acquire_timeout_secs: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        let b = Inst::open(&config, &app("hy")).await;
        let guard = a.try_lock_i64(1).await.unwrap().unwrap();

        let waiting = tokio::time::timeout(
            Duration::from_millis(200),
            b.lock_i64(1, Duration::from_secs(30)),
        )
        .await;
        assert!(waiting.is_err(), "还在等锁时被取消");
        assert!(b.try_lock_i64(2).await.unwrap().is_some());
        guard.release().await.unwrap();
    }

    /// 字符串 key：启动时建了 UNLOGGED 的锁表，插入不提交的行、靠主键冲突互斥；阻塞版超时、等到释放；
    /// 行永远不提交，持有中、释放后表里都看不到
    #[cfg(feature = "lock-row")]
    #[tokio::test]
    async fn string_key_uses_lock_table() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        assert!(table_exists(a.pool(), "hy_lock").await, "启动时建表");
        let persistence: String = sqlx::query_scalar(
            "SELECT relpersistence::text FROM pg_class WHERE oid = 'hy_lock'::regclass",
        )
        .fetch_one(&**a.pool())
        .await
        .unwrap();
        assert_eq!(persistence, "u", "UNLOGGED");

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

        let waited = waiter.await.unwrap().unwrap().unwrap();
        assert_eq!(count_rows(a.pool()).await, 0, "持有中的行没提交，看不到");
        waited.release().await.unwrap();
        assert_eq!(count_rows(a.pool()).await, 0, "释放后行跟着回滚");
    }

    #[cfg(feature = "lock-row")]
    async fn count_rows(pool: &hygiea_db::SqlxPgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM hy_lock")
            .fetch_one(&**pool)
            .await
            .unwrap()
    }

    /// 字符串 key：多个实例同时 try 同一个 key，只有一个拿到，其余的不等（最多 1 毫秒）返回 None
    #[cfg(feature = "lock-row")]
    #[tokio::test]
    async fn concurrent_try_lock_has_one_winner() {
        let server = start().await;
        let mut pools = Vec::new();
        for _ in 0..8 {
            pools.push(pool(&server).await);
        }
        for round in 0..3 {
            let mut tries = tokio::task::JoinSet::new();
            for p in &pools {
                let p = p.clone();
                tries.spawn(async move { p.try_lock(lock_key!(TestLockKey::Job, round)).await });
            }
            let mut winners = Vec::new();
            while let Some(result) = tries.join_next().await {
                winners.extend(result.unwrap().unwrap());
            }
            assert_eq!(winners.len(), 1, "第 {round} 轮");
            for guard in winners {
                guard.release().await.unwrap();
            }
        }
    }

    /// connect_lazy：启动时不连库、不建表，第一次用字符串 key 时建
    #[cfg(feature = "lock-row")]
    #[tokio::test]
    async fn lazy_pool_creates_table_on_first_use() {
        let server = start().await;
        let config = SqlxPgConfig {
            uses: uses(5),
            connect: connect(&server),
            pool: Some(PoolConfig {
                connect_lazy: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let lazy = Inst::open(&config, &app("lazy")).await;
        assert!(!table_exists(lazy.pool(), "lazy_lock").await);
        let guard = lazy.try_lock(lock_key!(TestLockKey::Job, 1)).await.unwrap();
        assert!(guard.is_some());
        assert!(table_exists(lazy.pool(), "lazy_lock").await);
    }

    /// 已有同名的表但列对不上：不动它，拒绝启动
    #[cfg(feature = "lock-row")]
    #[tokio::test]
    async fn mismatched_table_refuses_to_start() {
        let server = start().await;
        let other = pool(&server).await;
        sqlx::raw_sql("CREATE TABLE bad_lock (key integer PRIMARY KEY)")
            .execute(&**other.pool())
            .await
            .unwrap();
        let config = SqlxPgConfig {
            uses: uses(5),
            connect: connect(&server),
            ..Default::default()
        };
        let e = hygiea_db::DbResources::from_config(&config, &app("bad"))
            .await
            .unwrap_err();
        assert!(e.is(hygiea_db::RepoErr::LockTableSchemaMismatch), "{e:#}");
    }

    /// 已有同名的表、列对上但不是 UNLOGGED：不动它，拒绝启动
    #[cfg(feature = "lock-row")]
    #[tokio::test]
    async fn logged_table_refuses_to_start() {
        let server = start().await;
        let other = pool(&server).await;
        sqlx::raw_sql("CREATE TABLE logged_lock (key text PRIMARY KEY)")
            .execute(&**other.pool())
            .await
            .unwrap();
        let config = SqlxPgConfig {
            uses: uses(5),
            connect: connect(&server),
            ..Default::default()
        };
        let e = hygiea_db::DbResources::from_config(&config, &app("logged"))
            .await
            .unwrap_err();
        assert!(e.is(hygiea_db::RepoErr::LockTableSchemaMismatch), "{e:#}");
    }

    /// `[db.lock]` 写了 max_connections：锁单独建池（连接名 `hy-lock`），持有的锁不占连接池，
    /// 连接池只有 1 条连接也能照常查询；连接池关闭时锁的池一起关
    #[allow(clippy::needless_update)] // 不开 live-config 时 SqlxPgConfig 只有 connect、pool、lock
    #[tokio::test]
    async fn own_pool_keeps_locks_off_the_main_pool() {
        let server = start().await;
        let config = SqlxPgConfig {
            uses: uses(5),
            connect: connect(&server),
            pool: Some(PoolConfig {
                max_connections: Some(1),
                acquire_timeout_secs: Some(2),
                ..Default::default()
            }),
            lock: Some(hygiea_db::PgLockConfig {
                max_connections: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        let a = Inst::open(&config, &app("hy")).await;
        let first = a.try_lock_i64(1).await.unwrap().unwrap();
        let second = a.try_lock_i64(2).await.unwrap().unwrap();
        let held: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'hy-lock'",
        )
        .fetch_one(&**a.pool())
        .await
        .unwrap();
        assert_eq!(held, 2, "两把锁各占锁池的一条连接，连接池的那条还能用");
        first.release().await.unwrap();
        second.release().await.unwrap();

        a.close().await.unwrap();
        let observer = pool(&server).await;
        eventually(5, || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'hy-lock'",
            )
            .fetch_one(&**observer.pool())
            .await
            .unwrap()
                == 0
        })
        .await;
    }

    /// 只给锁用（`uses = 4`）：没有业务池，锁按默认的池参数单独建池（连接名 `hy-lock`），和别的实例照样互斥；
    /// 关掉后锁的池跟着关
    #[tokio::test]
    async fn lock_only_has_own_pool() {
        let server = start().await;
        let config = SqlxPgConfig {
            uses: uses(4),
            connect: connect(&server),
            ..Default::default()
        };
        let db = hygiea_db::DbResources::from_config(&config, &app("hy"))
            .await
            .unwrap();
        assert!(db.pool.is_none());
        let lock_client = db.lock_client.clone().unwrap();
        let other = pool(&server).await;
        let guard = lock_client.try_lock_i64(1).await.unwrap().unwrap();
        assert!(other.try_lock_i64(1).await.unwrap().is_none());
        let lock_conns = || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'hy-lock'",
            )
            .fetch_one(&**other.pool())
            .await
            .unwrap()
        };
        assert_eq!(lock_conns().await, 1);
        guard.release().await.unwrap();
        db.close().await.unwrap();
        eventually(5, || async { lock_conns().await == 0 }).await;
    }

    /// 锁表的 table_prefix 写空的、connect 只换 schema：表是 `coord.lock`，两个实例照样互斥
    #[cfg(feature = "lock-row")]
    #[tokio::test]
    async fn lock_table_prefix_and_connect() {
        let server = start().await;
        let admin = pool(&server).await;
        sqlx::raw_sql("CREATE SCHEMA coord")
            .execute(&**admin.pool())
            .await
            .unwrap();
        let config = SqlxPgConfig {
            uses: uses(5),
            connect: connect(&server),
            lock: Some(hygiea_db::PgLockConfig {
                table_prefix: Some(String::new()),
                connect: hygiea_db::PgConnectOverride {
                    schema_search_path: Some("coord".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let a = Inst::open(&config, &app("hy")).await;
        let b = Inst::open(&config, &app("hy")).await;
        assert!(table_exists(admin.pool(), "coord.lock").await);
        assert!(
            !table_exists(admin.pool(), "public.lock").await,
            "不在连接池的 schema 里"
        );

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
    }

    /// 没开 lock-row：字符串 key 返回 Unsupported，不建表；调用方显式哈希后走 try_lock_i64
    #[cfg(not(feature = "lock-row"))]
    #[tokio::test]
    async fn string_key_unsupported_without_lock_row() {
        use hygiea_core::lock::{LockErr, LockKeyI64};

        let server = start().await;
        let a = pool(&server).await;

        let e = a
            .try_lock(lock_key!(TestLockKey::Job, 1))
            .await
            .unwrap_err();
        assert!(e.is(LockErr::Unsupported), "{e:#}");
        let e = a
            .lock(lock_key!(TestLockKey::Job, 1), Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(e.is(LockErr::Unsupported), "{e:#}");

        let hashed = LockKeyI64::xxh3_64(&lock_key!(TestLockKey::Job, 1)).unwrap();
        let guard = a.try_lock_i64(hashed).await.unwrap().unwrap();
        assert_eq!(guard.key(), format!("advisory {}", hashed.get()));
        guard.release().await.unwrap();

        let table: bool = sqlx::query_scalar("SELECT to_regclass('hy_lock') IS NOT NULL")
            .fetch_one(&**a.pool())
            .await
            .unwrap();
        assert!(!table, "没开 lock-row 不建锁表");
    }
}

/// 动态配置（feature `live-config`）
#[cfg(feature = "live-config")]
#[container]
mod live_config {
    use hygiea_core::app::RegistryConfig;
    use hygiea_db::live_config::Live;
    use hygiea_db::{
        DbResources, PgConnectConfig, PgConnectOverride, PgLiveConfigConfig, RepoErr, SqlxPgConfig,
        SqlxPgPool,
    };
    use hygiea_test::container::{ContainerSpec, RunningContainer};
    use serde::{Deserialize, Serialize};

    use super::common::{LiveInst, uses};
    use super::*;

    type Inst = LiveInst<SqlxPgConfig>;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Live)]
    struct Job {
        enabled: bool,
        #[live(by_ref)]
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

    #[derive(Debug, Serialize, Deserialize)]
    struct Empty {}

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    struct All {
        sync: Job,
        cache: Cache,
    }

    /// 和 [`All`] 一样，按字段读（`#[derive(Live)]`）
    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Live)]
    struct LiveAll {
        #[live(nested)]
        sync: Job,
        cache: Cache,
    }

    const PASSWORD: &str = "hygiea-test";

    const PORT: u16 = 5432;

    /// 业务池模拟别的程序改表，动态配置走它自己的那条连接
    struct Env {
        server: RunningContainer,
        source: Inst,
        pool: SqlxPgPool,
    }

    impl Env {
        async fn new() -> Self {
            let server = ContainerSpec::new("postgres", "18")
                .env("POSTGRES_PASSWORD", PASSWORD)
                .port(PORT)
                .wait_log_times("database system is ready to accept connections", 2)
                .start()
                .await;
            let source = Inst::open(&sqlx_config(&server), &app()).await;
            let pool = source.pool().clone();
            Self {
                server,
                source,
                pool,
            }
        }

        /// 先绑一个空的 namespace：第一次 bind 才建表，测试要在 bind 前往表里写数据
        async fn start(&self) -> Inst {
            let source = self.source.clone();
            source.bind("_start", Empty {}).await.unwrap();
            source
        }

        async fn exec(&self, sql: &str) {
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&*self.pool)
                .await
                .unwrap();
        }
    }

    /// 应用名 `hy`：表是 hy_config 那三张；主池的连接叫 `hy`，live config 的小池叫 `hy-live-config`
    fn app() -> RegistryConfig {
        RegistryConfig {
            app_name: "hy".to_string(),
            ..Default::default()
        }
    }

    /// 轮询间隔很长：测试里的变化只能靠通知收到
    fn live_config() -> PgLiveConfigConfig {
        PgLiveConfigConfig {
            poll_interval_secs: Some(3600),
            ..Default::default()
        }
    }

    fn sqlx_config(server: &RunningContainer) -> SqlxPgConfig {
        SqlxPgConfig {
            uses: uses(3),
            connect: connect(server),
            live_config: Some(live_config()),
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

    /// 保留行数在 PG 上也生效：后台任务一启动就删到只剩最新的 N 行
    #[tokio::test]
    async fn history_is_trimmed() {
        let env = Env::new().await;
        let source = env.start().await;
        let _sync = source.bind("sync", Job::default()).await.unwrap();
        env.exec("UPDATE hy_config SET value = '[3]' WHERE namespace = 'sync' AND key = 'levels'")
            .await;
        // sync 3 条 I + 1 条 U
        let count = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM hy_config_history")
                .fetch_one(&*env.pool)
                .await
                .unwrap()
        };
        assert_eq!(count().await, 4);

        let mut config = sqlx_config(&env.server);
        config.live_config.as_mut().unwrap().history_keep_rows = Some(2);
        config
            .live_config
            .as_mut()
            .unwrap()
            .history_trim_interval_mins = Some(60);
        let restarted = Inst::open(&config, &app()).await;
        let _sync = restarted.bind("sync", Job::default()).await.unwrap();
        eventually(5, || async { count().await == 2 }).await;
        let op: String =
            sqlx::query_scalar("SELECT op FROM hy_config_history ORDER BY id DESC LIMIT 1")
                .fetch_one(&*env.pool)
                .await
                .unwrap();
        assert_eq!(op, "U", "留下的是最新的");
    }

    /// 改值靠 NOTIFY 推过来；history 记了数据库账号
    #[tokio::test]
    async fn update_is_pushed_by_notify() {
        let env = Env::new().await;
        let source = env.start().await;
        let mut sync = source.bind("sync", Job::default()).await.unwrap();
        assert_eq!(*sync.get(), Job::default());

        env.exec(
            "UPDATE hy_config SET value = '[3, 4]' WHERE namespace = 'sync' AND key = 'levels'",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), sync.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sync.get().levels, vec![3, 4]);

        let (op, by, revision): (String, String, i64) = sqlx::query_as(
            "SELECT op, changed_by, revision FROM hy_config_history ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&*env.pool)
        .await
        .unwrap();
        assert_eq!((op.as_str(), by.as_str(), revision), ("U", "postgres", 4));
    }

    /// bind_all 一次补完所有 namespace 的默认值（一条语句）：表里每个 key 都有，history 每个 key 一条 I，
    /// 值的类型（布尔、数组、字符串、数字）原样存成 JSON
    #[tokio::test]
    async fn bind_all_seeds_every_key() {
        let env = Env::new().await;
        let source = env.start().await;
        let all = source.bind_all(All::default()).await.unwrap();
        assert_eq!(*all.get(), All::default());

        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT namespace, key, value::text FROM hy_config \
             WHERE namespace IN ('sync', 'cache') ORDER BY namespace, key",
        )
        .fetch_all(&*env.pool)
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
        let inserted: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM hy_config_history WHERE namespace IN ('sync', 'cache') AND op = 'I'",
        )
        .fetch_one(&*env.pool)
        .await
        .unwrap();
        assert_eq!(inserted, 4);
    }

    /// 一个事务里改多个 key、删 key：都能收到，删掉的退回默认值
    #[tokio::test]
    async fn transaction_with_many_changes() {
        let env = Env::new().await;
        let source = env.start().await;
        let all = source.bind_all(All::default()).await.unwrap();

        env.exec(
            "BEGIN; \
             UPDATE hy_config SET value = 'true' WHERE namespace = 'sync' AND key = 'enabled'; \
             UPDATE hy_config SET value = '5' WHERE namespace = 'cache' AND key = 'ttl_secs'; \
             DELETE FROM hy_config WHERE namespace = 'sync' AND key = 'levels'; \
             COMMIT;",
        )
        .await;
        eventually(5, || async {
            let all = all.get();
            all.sync.enabled && all.cache.ttl_secs == 5
        })
        .await;
        assert_eq!(all.get().sync.levels, vec![1, 2]);
    }

    /// 回滚的修改不发通知，值不变
    #[tokio::test]
    async fn rolled_back_change_is_ignored() {
        let env = Env::new().await;
        let source = env.start().await;
        let mut sync = source.bind("sync", Job::default()).await.unwrap();

        env.exec(
            "BEGIN; UPDATE hy_config SET value = 'true' WHERE namespace = 'sync' AND key = 'enabled'; ROLLBACK;",
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1500), sync.changed())
                .await
                .is_err()
        );
        assert!(!sync.get().enabled);
    }

    /// 几个实例同时第一次 bind（同时建表），都成功
    #[tokio::test]
    async fn concurrent_startup_creates_tables_once() {
        let env = Env::new().await;
        let mut handles = Vec::new();
        for _ in 0..4 {
            let pool = Inst::open(&sqlx_config(&env.server), &app()).await;
            handles.push(tokio::spawn(async move {
                pool.bind("cache", Cache::default()).await.map(|_| ())
            }));
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
    }

    /// 已经建好的表重启时直接用，值不动；表加了一列：拒绝启动
    #[tokio::test]
    async fn schema_check_on_restart() {
        let env = Env::new().await;
        let source = env.start().await;
        source.bind("cache", Cache::default()).await.unwrap();
        env.exec("UPDATE hy_config SET value = '42' WHERE namespace = 'cache'")
            .await;

        let restarted = Inst::open(&sqlx_config(&env.server), &app()).await;
        let cache = restarted.bind("cache", Cache::default()).await.unwrap();
        assert_eq!(cache.get().ttl_secs, 42, "表里改过的值不能被默认值覆盖");
        restarted.close().await.unwrap();

        env.exec("ALTER TABLE hy_config ADD COLUMN note text").await;
        let restarted = Inst::open(&sqlx_config(&env.server), &app()).await;
        let e = restarted.bind("cache", Cache::default()).await.unwrap_err();
        assert!(e.is(RepoErr::LiveConfigSchemaMismatch), "{e:#}");
    }

    /// 同名函数不是 hygiea 建的（没有标记）：不动它，拒绝启动
    #[tokio::test]
    async fn foreign_function_refuses() {
        let env = Env::new().await;
        env.exec(
            "CREATE FUNCTION hy_config_touch() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RETURN NEW; END $$;",
        )
        .await;
        let e = env
            .source
            .bind("cache", Cache::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigSchemaMismatch), "{e:#}");
        let tables: bool = sqlx::query_scalar("SELECT to_regclass('hy_config') IS NULL")
            .fetch_one(&*env.pool)
            .await
            .unwrap();
        assert!(tables, "拒绝时一张表都不建");
    }

    /// 连接名：主池用应用名，小池加 `-live-config`；params 里不能自己写 application_name
    #[tokio::test]
    async fn application_names_follow_app_name() {
        let env = Env::new().await;
        env.start().await;
        let names = |pool: SqlxPgPool| async move {
            sqlx::query_scalar::<_, String>(
                "SELECT DISTINCT application_name FROM pg_stat_activity \
                 WHERE application_name IN ('hy', 'hy-live-config') ORDER BY 1",
            )
            .fetch_all(&*pool)
            .await
            .unwrap()
        };
        assert_eq!(names(env.pool.clone()).await, vec!["hy", "hy-live-config"]);

        let mut config = sqlx_config(&env.server);
        config.connect.params = "application_name=replica".to_string();
        let e = DbResources::from_config(&config, &app()).await.unwrap_err();
        assert!(e.is(hygiea_core::app::AppErr::InvalidConfig), "{e:#}");
    }

    /// 换应用名：表名前缀、触发器、通知的 channel 都跟着换
    #[tokio::test]
    async fn table_prefix_follows_app_name() {
        let env = Env::new().await;
        let cfg = RegistryConfig {
            app_name: "cfg".to_string(),
            ..Default::default()
        };
        let pool = Inst::open(&sqlx_config(&env.server), &cfg).await;
        let cache = pool.bind("cache", Cache::default()).await.unwrap();

        env.exec("UPDATE cfg_config SET value = '7' WHERE namespace = 'cache'")
            .await;
        eventually(5, || async { cache.get().ttl_secs == 7 }).await;
    }

    /// table_prefix 比应用名优先，`foo` 和 `foo_` 一样；`""` 不加前缀
    #[tokio::test]
    async fn custom_table_prefix() {
        let env = Env::new().await;
        for (prefix, table) in [
            ("foo", "foo_config"),
            ("foo_", "foo_config"),
            ("", "config"),
        ] {
            let mut config = sqlx_config(&env.server);
            config.live_config.as_mut().unwrap().table_prefix = Some(prefix.to_string());
            let pool = Inst::open(&config, &app()).await;
            let cache = pool.bind("cache", Cache::default()).await.unwrap();
            let ttl = cache.get().ttl_secs + 1;
            env.exec(&format!(
                "UPDATE {table} SET value = '{ttl}' WHERE namespace = 'cache'"
            ))
            .await;
            eventually(5, || async { cache.get().ttl_secs == ttl }).await;
            pool.close().await.unwrap();
        }
    }

    /// `[db.live_config.connect]` 只写用户、密码、schema，其余沿用 `[db.connect]`：表建在那个 schema 里、
    /// 用那个用户连，改表照样推过来
    #[tokio::test]
    async fn own_connection_overrides_some_fields() {
        let env = Env::new().await;
        env.exec(
            "CREATE ROLE cfg_user LOGIN PASSWORD 'cfg-pass'; \
             CREATE SCHEMA cfg AUTHORIZATION cfg_user;",
        )
        .await;
        let mut config = sqlx_config(&env.server);
        config.live_config.as_mut().unwrap().connect = PgConnectOverride {
            username: Some("cfg_user".to_string()),
            password: Some("cfg-pass".to_string()),
            schema_search_path: Some("cfg".to_string()),
            ..Default::default()
        };
        let pool = Inst::open(&config, &app()).await;
        let cache = pool.bind("cache", Cache::default()).await.unwrap();

        let (in_cfg, in_public): (bool, bool) = sqlx::query_as(
            "SELECT to_regclass('cfg.hy_config') IS NOT NULL, to_regclass('public.hy_config') IS NOT NULL",
        )
        .fetch_one(&*env.pool)
        .await
        .unwrap();
        assert!(in_cfg && !in_public, "表建在 cfg 里");
        let user: String = sqlx::query_scalar(
            "SELECT usename::text FROM pg_stat_activity WHERE application_name = 'hy-live-config'",
        )
        .fetch_one(&*env.pool)
        .await
        .unwrap();
        assert_eq!(user, "cfg_user");

        env.exec("UPDATE cfg.hy_config SET value = '9' WHERE namespace = 'cache'")
            .await;
        eventually(5, || async { cache.get().ttl_secs == 9 }).await;
    }

    /// 只给动态配置用（`uses = 2`）：没有业务池，照样建表、靠通知收到修改
    #[tokio::test]
    async fn live_config_only_without_pool() {
        let env = Env::new().await;
        let config = SqlxPgConfig {
            uses: uses(2),
            connect: connect(&env.server),
            live_config: Some(live_config()),
            ..Default::default()
        };
        let source = Inst::open(&config, &app()).await;
        assert!(source.0.pool.is_none());
        let cache = source.bind("cache", Cache::default()).await.unwrap();
        env.exec("UPDATE hy_config SET value = '5' WHERE namespace = 'cache'")
            .await;
        eventually(5, || async { cache.get().ttl_secs == 5 }).await;
        source.close().await.unwrap();
    }

    /// 两个 schema 里同名的表：channel 带 schema 名（`<schema>.config`），各收各的
    #[tokio::test]
    async fn channel_is_per_schema() {
        let env = Env::new().await;
        env.exec("CREATE SCHEMA a; CREATE SCHEMA b;").await;
        let bind = |schema: &'static str| {
            let mut config = sqlx_config(&env.server);
            config.live_config.as_mut().unwrap().table_prefix = Some(String::new());
            config
                .live_config
                .as_mut()
                .unwrap()
                .connect
                .schema_search_path = Some(schema.to_string());
            async move {
                let pool = Inst::open(&config, &app()).await;
                pool.bind("cache", Cache::default()).await.unwrap()
            }
        };
        let (a, b) = (bind("a").await, bind("b").await);
        let source: String = sqlx::query_scalar(
            "SELECT p.prosrc FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'a' AND p.proname = 'config_record'",
        )
        .fetch_one(&*env.pool)
        .await
        .unwrap();
        assert!(source.contains("pg_notify('a.config'"), "{source}");

        env.exec("UPDATE a.config SET value = '1' WHERE namespace = 'cache'")
            .await;
        eventually(5, || async { a.get().ttl_secs == 1 }).await;
        assert_eq!(b.get().ttl_secs, 60, "b 的表没改");
        env.exec("UPDATE b.config SET value = '2' WHERE namespace = 'cache'")
            .await;
        eventually(5, || async { b.get().ttl_secs == 2 }).await;
        assert_eq!(a.get().ttl_secs, 1);
    }

    /// 函数发通知的 channel 和现在的对不上（schema 改了名，或者旧版建的）：拒绝启动
    #[tokio::test]
    async fn stale_channel_refuses() {
        let env = Env::new().await;
        env.exec("CREATE SCHEMA s1").await;
        let config = |schema: &str| {
            let mut config = sqlx_config(&env.server);
            config
                .live_config
                .as_mut()
                .unwrap()
                .connect
                .schema_search_path = Some(schema.to_string());
            config
        };
        let pool = Inst::open(&config("s1"), &app()).await;
        pool.bind("cache", Cache::default()).await.unwrap();
        pool.close().await.unwrap();

        env.exec("ALTER SCHEMA s1 RENAME TO s2").await;
        let pool = Inst::open(&config("s2"), &app()).await;
        let e = pool.bind("cache", Cache::default()).await.unwrap_err();
        assert!(e.is(RepoErr::LiveConfigSchemaMismatch), "{e:#}");
    }

    /// 监听连接被服务端断掉：重连后对一遍版本，断线期间的修改也能拿到
    #[tokio::test]
    async fn reconnects_after_connection_killed() {
        let env = Env::new().await;
        let source = env.start().await;
        let cache = source.bind("cache", Cache::default()).await.unwrap();

        let killed: Vec<bool> = sqlx::query_scalar(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE application_name = 'hy-live-config'",
        )
        .fetch_all(&*env.pool)
        .await
        .unwrap();
        assert_eq!(killed, vec![true], "应该正好断掉监听的那一条连接");
        env.exec("UPDATE hy_config SET value = '9' WHERE namespace = 'cache'")
            .await;
        eventually(15, || async { cache.get().ttl_secs == 9 }).await;
    }

    /// 监听连接的 pid（`usename` 是数据库用户）
    async fn listener_pid(env: &Env, user: &str) -> i32 {
        sqlx::query_scalar(
            "SELECT pid FROM pg_stat_activity \
             WHERE application_name = 'hy-live-config' AND usename = $1",
        )
        .bind(user)
        .fetch_one(&*env.pool)
        .await
        .unwrap()
    }

    /// DBA 在用户上设了很短的超时：监听连接空闲超过 idle_session_timeout 也不被断开（还是同一条连接），
    /// 改表照样靠通知收到；同一个用户的连接池照旧受 statement_timeout 限制
    #[tokio::test]
    async fn dba_timeouts_do_not_cut_listener() {
        let env = Env::new().await;
        env.start().await;
        env.exec(
            "CREATE ROLE strict LOGIN SUPERUSER PASSWORD 'strict'; \
             ALTER ROLE strict SET idle_session_timeout = '300ms'; \
             ALTER ROLE strict SET statement_timeout = '100ms'; \
             ALTER ROLE strict SET transaction_timeout = '100ms'; \
             ALTER ROLE strict SET lock_timeout = '100ms'",
        )
        .await;
        let config = SqlxPgConfig {
            uses: uses(3),
            connect: PgConnectConfig {
                username: "strict".to_string(),
                password: "strict".to_string(),
                ..connect(&env.server)
            },
            live_config: Some(live_config()),
            ..Default::default()
        };
        let strict = Inst::open(&config, &app()).await;
        let mut cache = strict.bind("cache", Cache::default()).await.unwrap();
        let pid = listener_pid(&env, "strict").await;

        let slow = sqlx::query("SELECT pg_sleep(0.5)")
            .execute(&**strict.pool())
            .await;
        assert!(slow.is_err(), "连接池仍受 statement_timeout 限制");

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(listener_pid(&env, "strict").await, pid, "监听连接没被断开");
        env.exec("UPDATE hy_config SET value = '7' WHERE namespace = 'cache'")
            .await;
        tokio::time::timeout(Duration::from_secs(5), cache.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cache.get().ttl_secs, 7);
        assert_eq!(listener_pid(&env, "strict").await, pid);
    }

    /// 语句级的错误（这里是表被改名，补默认值失败）不重建 listener：小池只有一条连接，重建也拿回同一条，
    /// pid 看不出来，所以看日志。表改回来后照样靠通知收到
    #[tokio::test]
    async fn statement_error_keeps_listener() {
        let env = Env::new().await;
        let source = env.start().await;
        let mut cache = source.bind("cache", Cache::default()).await.unwrap();

        let (logs, _guard) = test_support::logs::capture();
        env.exec("ALTER TABLE hy_config RENAME TO hy_config_moved")
            .await;
        let e = source.bind("sync", Job::default()).await.unwrap_err();
        assert!(e.is(RepoErr::InsertFailed), "{e:#}");
        env.exec("ALTER TABLE hy_config_moved RENAME TO hy_config")
            .await;
        let text = logs.text();
        assert!(text.contains("live config: query failed"), "{text}");
        assert!(!text.contains("rebuilding"), "{text}");

        env.exec("UPDATE hy_config SET value = '8' WHERE namespace = 'cache'")
            .await;
        tokio::time::timeout(Duration::from_secs(5), cache.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cache.get().ttl_secs, 8);
    }

    /// `#[derive(Live)]` 的句柄：改表后字段方法读到新值，changed_by 只等盯的字段，on_change_by 回调拿到新值
    #[tokio::test]
    async fn live_handle_reads_updates() {
        let env = Env::new().await;
        let source = env.start().await;
        let runtime = source.bind_all(LiveAll::default()).await.unwrap();
        let mut sync = runtime.live().sync();
        let start_times = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let pushed = start_times.clone();
        let _guard = sync.on_change_by(
            |c| &c.start_time,
            move |t| pushed.lock().unwrap().push(t.clone()),
        );

        env.exec("UPDATE hy_config SET value = '[3]' WHERE namespace = 'sync' AND key = 'levels'")
            .await;
        eventually(5, || async { *sync.levels() == vec![3] }).await;
        let waited = tokio::time::timeout(
            Duration::from_millis(300),
            sync.changed_by(|c| &c.start_time),
        )
        .await;
        assert!(waited.is_err(), "start_time 没变，changed_by 不应该返回");

        env.exec(
            "UPDATE hy_config SET value = '\"02:00\"' WHERE namespace = 'sync' AND key = 'start_time'",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), sync.changed_by(|c| &c.start_time))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sync.start_time(), "02:00");
        assert_eq!(runtime.live().snapshot().sync.start_time, "02:00");
        eventually(5, || async {
            *start_times.lock().unwrap() == vec!["02:00".to_string()]
        })
        .await;
    }
}
