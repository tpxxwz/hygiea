//! 连接池的动态配置能力（feature `live-config`）的集成测试。
//!
//! SQLite 用临时文件，直接跑；PostgreSQL 是 container 层，要 Docker：
//! ```sh
//! cargo test -p hygiea-db --all-features --test live_config
//! cargo test -p hygiea-db --all-features --test live_config -- --ignored ::container::
//! ```

#![cfg(all(feature = "live-config", feature = "postgres", feature = "sqlite"))]

use std::future::Future;
use std::time::Duration;

use hygiea_test::container;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Serialize, Deserialize)]
struct Empty {}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct All {
    sync: Job,
    cache: Cache,
}

/// 条件在 `secs` 秒内成立
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

mod sqlite {
    use hygiea_core::app::{AppContext, ConfigResource};
    use hygiea_db::{
        LiveConfigOptions, PoolConfig, RepoErr, SqliteConnectConfig, SqlxSqliteConfig,
        SqlxSqlitePool,
    };
    use tempfile::TempDir;

    use super::*;

    /// 应用名 `hy`：表是 hy_config、hy_config_revision、hy_config_history
    fn app() -> AppContext {
        AppContext {
            app_name: "hy".to_string(),
        }
    }

    fn enabled() -> LiveConfigOptions {
        LiveConfigOptions {
            enabled: true,
            ..Default::default()
        }
    }

    /// 一个临时库。连接池本身模拟别的程序改表，动态配置走它自己的那条连接
    struct Env {
        _dir: TempDir,
        config: SqlxSqliteConfig,
        pool: SqlxSqlitePool,
    }

    impl Env {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let config = SqlxSqliteConfig {
                connect: SqliteConnectConfig {
                    database: dir.path().join("dyn.db").display().to_string(),
                    ..Default::default()
                },
                live_config: enabled(),
                ..Default::default()
            };
            let pool = SqlxSqlitePool::from_config(&config, &app()).await.unwrap();
            Self {
                _dir: dir,
                config,
                pool,
            }
        }

        /// 同一个库再建一个连接池，相当于重启程序
        async fn restart(&self) -> SqlxSqlitePool {
            SqlxSqlitePool::from_config(&self.config, &app())
                .await
                .unwrap()
        }

        /// 先绑一个空的 namespace：第一次 bind 才建表，测试要在 bind 前往表里写数据
        async fn start(&self) -> SqlxSqlitePool {
            let source = self.pool.clone();
            source.live_config("_start", Empty {}).await.unwrap();
            source
        }

        async fn exec(&self, sql: &'static str) {
            sqlx::query(sql).execute(&*self.pool).await.unwrap();
        }

        async fn value(&self, namespace: &str, key: &str) -> Option<String> {
            sqlx::query_scalar("SELECT value FROM hy_config WHERE namespace = ?1 AND key = ?2")
                .bind(namespace)
                .bind(key)
                .fetch_optional(&*self.pool)
                .await
                .unwrap()
        }
    }

    /// 表里没有时按默认值逐个 key 写进去，history 里记 I，拿到的就是默认值
    #[tokio::test]
    async fn bind_seeds_defaults() {
        let env = Env::new().await;
        let source = env.start().await;
        let sync = source.live_config("sync", Job::default()).await.unwrap();

        assert_eq!(*sync.get(), Job::default());
        assert_eq!(env.value("sync", "levels").await.as_deref(), Some("[1,2]"));
        let history: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT key, op, revision FROM hy_config_history WHERE namespace = 'sync' ORDER BY id",
        )
        .fetch_all(&*env.pool)
        .await
        .unwrap();
        assert_eq!(history.len(), 3);
        assert!(history.iter().all(|(_, op, _)| op == "I"));
        assert_eq!(history.last().map(|row| row.2), Some(3));
    }

    /// 表里已经有的 key 以表为准，默认值不覆盖
    #[tokio::test]
    async fn table_value_wins_over_default() {
        let env = Env::new().await;
        let source = env.start().await;
        env.exec(
            "INSERT INTO hy_config (namespace, key, value) VALUES ('sync', 'enabled', 'true')",
        )
        .await;

        let sync = source.live_config("sync", Job::default()).await.unwrap();
        assert!(sync.get().enabled);
        assert_eq!(env.value("sync", "enabled").await.as_deref(), Some("true"));
    }

    /// 别的连接改值：轮询发现，changed 返回，get 拿到新值；history 记 U 带新旧值
    #[tokio::test]
    async fn update_is_picked_up_by_polling() {
        let env = Env::new().await;
        let source = env.start().await;
        let mut sync = source.live_config("sync", Job::default()).await.unwrap();

        env.exec("UPDATE hy_config SET value = '[3]' WHERE namespace = 'sync' AND key = 'levels'")
            .await;
        tokio::time::timeout(Duration::from_secs(5), sync.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sync.get().levels, vec![3]);

        let (op, old, new): (String, String, String) = sqlx::query_as(
            "SELECT op, old_value, new_value FROM hy_config_history ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&*env.pool)
        .await
        .unwrap();
        assert_eq!(
            (op.as_str(), old.as_str(), new.as_str()),
            ("U", "[1,2]", "[3]")
        );
    }

    /// 值没变的 UPDATE 不算变更：版本号不动，不写 history
    #[tokio::test]
    async fn same_value_update_is_not_a_change() {
        let env = Env::new().await;
        let source = env.start().await;
        source.live_config("sync", Job::default()).await.unwrap();

        env.exec("UPDATE hy_config SET value = value WHERE namespace = 'sync'")
            .await;
        let revision: i64 =
            sqlx::query_scalar("SELECT revision FROM hy_config_revision WHERE namespace = 'sync'")
                .fetch_one(&*env.pool)
                .await
                .unwrap();
        assert_eq!(revision, 3);
    }

    /// 删掉的 key 退回默认值
    #[tokio::test]
    async fn deleted_key_falls_back_to_default() {
        let env = Env::new().await;
        let source = env.start().await;
        env.exec("INSERT INTO hy_config (namespace, key, value) VALUES ('sync', 'start_time', '\"02:00\"')")
            .await;
        let sync = source.live_config("sync", Job::default()).await.unwrap();
        assert_eq!(sync.get().start_time, "02:00");

        env.exec("DELETE FROM hy_config WHERE namespace = 'sync' AND key = 'start_time'")
            .await;
        eventually(5, || async { sync.get().start_time == "01:00" }).await;
    }

    /// 改成转不了的值：保留旧值；改回合法的值后恢复更新
    #[tokio::test]
    async fn bad_value_keeps_old() {
        let env = Env::new().await;
        let source = env.start().await;
        let sync = source.live_config("sync", Job::default()).await.unwrap();

        env.exec(
            "UPDATE hy_config SET value = '\"abc\"' WHERE namespace = 'sync' AND key = 'enabled'",
        )
        .await;
        // 等过两轮轮询，值还是旧的
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(*sync.get(), Job::default());

        env.exec(
            "UPDATE hy_config SET value = 'true' WHERE namespace = 'sync' AND key = 'enabled'",
        )
        .await;
        eventually(5, || async { sync.get().enabled }).await;
    }

    /// 启动时表里的值就转不了：bind 返回 DecodeFailed
    #[tokio::test]
    async fn bind_fails_on_bad_value() {
        let env = Env::new().await;
        let source = env.start().await;
        env.exec("INSERT INTO hy_config (namespace, key, value) VALUES ('sync', 'enabled', '1.5')")
            .await;

        let e = source
            .live_config("sync", Job::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigDecodeFailed), "{e:#}");
    }

    /// 默认值不是 JSON 对象：DefaultsInvalid
    #[tokio::test]
    async fn defaults_must_be_object() {
        let env = Env::new().await;
        let source = env.start().await;
        let e = source.live_config("list", vec![1, 2]).await.unwrap_err();
        assert!(e.is(RepoErr::LiveConfigDefaultsInvalid), "{e:#}");

        #[derive(Debug, Serialize, Deserialize)]
        struct Root {
            flag: bool,
        }
        let e = source
            .live_config_all(Root { flag: true })
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigDefaultsInvalid), "{e:#}");
    }

    /// bind_all：第一层是 namespace；只改一个 namespace，另一个的值不变
    #[tokio::test]
    async fn bind_all_namespaces() {
        let env = Env::new().await;
        let source = env.start().await;
        let all = source.live_config_all(All::default()).await.unwrap();
        assert_eq!(env.value("cache", "ttl_secs").await.as_deref(), Some("60"));

        env.exec(
            "UPDATE hy_config SET value = '120' WHERE namespace = 'cache' AND key = 'ttl_secs'",
        )
        .await;
        eventually(5, || async { all.get().cache.ttl_secs == 120 }).await;
        assert_eq!(all.get().sync, Job::default());
    }

    /// view：从整份配置里取一块交给只用它的地方，取到的是最新的；拿着的快照不会被后来的新值改掉
    #[tokio::test]
    async fn view_follows_latest() {
        let env = Env::new().await;
        let source = env.start().await;
        let all = source.live_config_all(All::default()).await.unwrap();
        let cache = all.view(|c| &c.cache);
        let before = cache.get();
        assert_eq!(before.ttl_secs, 60);

        env.exec(
            "UPDATE hy_config SET value = '300' WHERE namespace = 'cache' AND key = 'ttl_secs'",
        )
        .await;
        eventually(5, || async { cache.get().ttl_secs == 300 }).await;
        assert_eq!(before.ttl_secs, 60, "拿着的是旧的那一版");
        assert_eq!(cache.clone().get().ttl_secs, 300);
    }

    /// struct 加了字段：再 bind 时只补新的 key，已有的不动
    #[tokio::test]
    async fn new_field_is_seeded() {
        #[derive(Serialize, Deserialize)]
        struct V1 {
            ttl_secs: u64,
        }
        #[derive(Serialize, Deserialize)]
        struct V2 {
            ttl_secs: u64,
            max_items: u32,
        }

        let env = Env::new().await;
        let source = env.start().await;
        source
            .live_config("cache", V1 { ttl_secs: 60 })
            .await
            .unwrap();
        env.exec(
            "UPDATE hy_config SET value = '90' WHERE namespace = 'cache' AND key = 'ttl_secs'",
        )
        .await;

        let v2 = source
            .live_config(
                "cache",
                V2 {
                    ttl_secs: 60,
                    max_items: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(v2.get().ttl_secs, 90);
        assert_eq!(v2.get().max_items, 10);
    }

    /// 改 key 名算删掉旧的、插入新的
    #[tokio::test]
    async fn rename_is_delete_and_insert() {
        let env = Env::new().await;
        let source = env.start().await;
        source.live_config("cache", Cache::default()).await.unwrap();

        env.exec("UPDATE hy_config SET key = 'ttl' WHERE namespace = 'cache' AND key = 'ttl_secs'")
            .await;
        let ops: Vec<(String, String)> = sqlx::query_as(
            "SELECT key, op FROM hy_config_history WHERE namespace = 'cache' ORDER BY id DESC LIMIT 2",
        )
        .fetch_all(&*env.pool)
        .await
        .unwrap();
        assert_eq!(
            ops,
            vec![
                ("ttl".to_string(), "I".to_string()),
                ("ttl_secs".to_string(), "D".to_string())
            ]
        );
    }

    /// 连接池关闭后 bind 报 SourceClosed：已经起了后台任务的、还没起的都一样
    #[tokio::test]
    async fn closed_source() {
        let started = Env::new().await;
        started
            .pool
            .live_config("sync", Job::default())
            .await
            .unwrap();
        started.pool.close().await.unwrap();
        let e = started
            .pool
            .live_config("sync", Job::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigClosed), "{e:#}");

        let never_started = Env::new().await;
        never_started.pool.close().await.unwrap();
        let e = never_started
            .pool
            .live_config("sync", Job::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigClosed), "{e:#}");
    }

    /// 库里已经有一张同名、列不一样的表：不动它，拒绝启动
    #[tokio::test]
    async fn table_mismatch_refuses() {
        let env = Env::new().await;
        env.exec("CREATE TABLE hy_config (id INTEGER PRIMARY KEY, name TEXT)")
            .await;
        let e = env
            .pool
            .live_config("cache", Cache::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigSchemaMismatch), "{e:#}");
        // 别人的表原样还在，也没有建 revision 表
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('hy_config') ORDER BY cid")
                .fetch_all(&*env.pool)
                .await
                .unwrap();
        assert_eq!(columns, vec!["id", "name"]);
        let revision: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE name = 'hy_config_revision'",
        )
        .fetch_one(&*env.pool)
        .await
        .unwrap();
        assert_eq!(revision, 0);
    }

    /// 表是 hygiea 的，但加了一列：也算对不上，拒绝启动
    #[tokio::test]
    async fn changed_columns_refuse() {
        let env = Env::new().await;
        env.start().await;
        env.exec("ALTER TABLE hy_config ADD COLUMN note TEXT").await;
        let e = env
            .restart()
            .await
            .live_config("cache", Cache::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigSchemaMismatch), "{e:#}");
    }

    /// 同名的触发器没有 hygiea 的标记：是别人的，不动它，拒绝启动
    #[tokio::test]
    async fn foreign_trigger_refuses() {
        let env = Env::new().await;
        env.exec("CREATE TABLE other (id INTEGER)").await;
        env.exec("CREATE TRIGGER hy_config_insert AFTER INSERT ON other BEGIN SELECT 1; END")
            .await;
        let e = env
            .pool
            .live_config("cache", Cache::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigSchemaMismatch), "{e:#}");
        let sql: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = 'hy_config_insert'")
                .fetch_one(&*env.pool)
                .await
                .unwrap();
        assert!(sql.contains("ON other"), "别人的触发器要原样留着: {sql}");
    }

    /// 表在、触发器被删了一个：不自动补，拒绝启动，表里的配置不动
    #[tokio::test]
    async fn missing_trigger_refuses() {
        let env = Env::new().await;
        let source = env.start().await;
        source.live_config("cache", Cache::default()).await.unwrap();
        env.exec("UPDATE hy_config SET value = '99' WHERE namespace = 'cache'")
            .await;
        env.exec("DROP TRIGGER hy_config_delete").await;

        let e = env
            .restart()
            .await
            .live_config("cache", Cache::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigSchemaMismatch), "{e:#}");
        assert_eq!(env.value("cache", "ttl_secs").await.as_deref(), Some("99"));
    }

    /// 主池只有一个名额且被占着时，动态配置照样能用：它走自己的小池，不占主池的名额
    #[tokio::test]
    async fn live_config_does_not_use_main_pool() {
        let dir = tempfile::tempdir().unwrap();
        let config = SqlxSqliteConfig {
            connect: SqliteConnectConfig {
                database: dir.path().join("dedicated.db").display().to_string(),
                ..Default::default()
            },
            pool: PoolConfig {
                max_connections: Some(1),
                acquire_timeout_secs: Some(1),
                ..Default::default()
            },
            live_config: enabled(),
        };
        let pool = SqlxSqlitePool::from_config(&config, &app()).await.unwrap();
        let _held = pool.acquire().await.unwrap();

        let cache = pool.live_config("cache", Cache::default()).await.unwrap();
        assert_eq!(*cache.get(), Cache::default());
    }

    /// 配置里没开：调用报 LiveConfigDisabled，也不建表
    #[tokio::test]
    async fn disabled_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = SqlxSqliteConfig {
            connect: SqliteConnectConfig {
                database: dir.path().join("off.db").display().to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let pool = SqlxSqlitePool::from_config(&config, &app()).await.unwrap();
        let e = pool
            .live_config("cache", Cache::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigDisabled), "{e:#}");
        let e = pool.live_config_all(All::default()).await.unwrap_err();
        assert!(e.is(RepoErr::LiveConfigDisabled), "{e:#}");
        let tables: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name LIKE 'hy_config%'")
                .fetch_one(&*pool)
                .await
                .unwrap();
        assert_eq!(tables, 0);
    }

    /// 不 bind 就不建表
    #[tokio::test]
    async fn no_tables_until_bind() {
        let env = Env::new().await;
        let tables: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name LIKE 'hy_config%'")
                .fetch_one(&*env.pool)
                .await
                .unwrap();
        assert_eq!(tables, 0);
    }
}

/// 用代码启动的官方 postgres 镜像
#[container]
mod postgres {
    use hygiea_core::app::{AppContext, ConfigResource};
    use hygiea_db::{
        LiveConfigOptions, PgConnectConfig, RepoErr, SeaOrmPgConfig, SeaOrmPgPool, SqlxPgConfig,
        SqlxPgPool,
    };
    use hygiea_test::container::{ContainerSpec, RunningContainer};

    use super::*;

    const PASSWORD: &str = "hygiea-test";
    const PORT: u16 = 5432;

    /// 连接池本身模拟别的程序改表，动态配置走它自己的那条连接
    struct Env {
        server: RunningContainer,
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
            let pool = SqlxPgPool::from_config(&sqlx_config(&server), &app())
                .await
                .unwrap();
            Self { server, pool }
        }

        /// 先绑一个空的 namespace：第一次 bind 才建表，测试要在 bind 前往表里写数据
        async fn start(&self) -> SqlxPgPool {
            let source = self.pool.clone();
            source.live_config("_start", Empty {}).await.unwrap();
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
    fn app() -> AppContext {
        AppContext {
            app_name: "hy".to_string(),
        }
    }

    /// 轮询间隔很长：测试里的变化只能靠通知收到
    fn live_config() -> LiveConfigOptions {
        LiveConfigOptions {
            enabled: true,
            poll_interval_secs: Some(3600),
        }
    }

    fn sqlx_config(server: &RunningContainer) -> SqlxPgConfig {
        SqlxPgConfig {
            connect: connect(server),
            live_config: live_config(),
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

    /// 改值靠 NOTIFY 推过来；history 记了数据库账号
    #[tokio::test]
    async fn update_is_pushed_by_notify() {
        let env = Env::new().await;
        let source = env.start().await;
        let mut sync = source.live_config("sync", Job::default()).await.unwrap();
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
        let all = source.live_config_all(All::default()).await.unwrap();
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
        let all = source.live_config_all(All::default()).await.unwrap();

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
        let mut sync = source.live_config("sync", Job::default()).await.unwrap();

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
            let pool = SqlxPgPool::from_config(&sqlx_config(&env.server), &app())
                .await
                .unwrap();
            handles.push(tokio::spawn(async move {
                pool.live_config("cache", Cache::default())
                    .await
                    .map(|_| ())
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
        source.live_config("cache", Cache::default()).await.unwrap();
        env.exec("UPDATE hy_config SET value = '42' WHERE namespace = 'cache'")
            .await;

        let restarted = SqlxPgPool::from_config(&sqlx_config(&env.server), &app())
            .await
            .unwrap();
        let cache = restarted
            .live_config("cache", Cache::default())
            .await
            .unwrap();
        assert_eq!(cache.get().ttl_secs, 42, "表里改过的值不能被默认值覆盖");
        restarted.close().await.unwrap();

        env.exec("ALTER TABLE hy_config ADD COLUMN note text").await;
        let restarted = SqlxPgPool::from_config(&sqlx_config(&env.server), &app())
            .await
            .unwrap();
        let e = restarted
            .live_config("cache", Cache::default())
            .await
            .unwrap_err();
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
            .pool
            .live_config("cache", Cache::default())
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
        let e = SqlxPgPool::from_config(&config, &app()).await.unwrap_err();
        assert!(e.is(hygiea_core::app::AppErr::InvalidConfig), "{e:#}");
    }

    /// 换应用名：表名前缀、触发器、通知的 channel 都跟着换
    #[tokio::test]
    async fn table_prefix_follows_app_name() {
        let env = Env::new().await;
        let cfg = AppContext {
            app_name: "cfg".to_string(),
        };
        let pool = SqlxPgPool::from_config(&sqlx_config(&env.server), &cfg)
            .await
            .unwrap();
        let cache = pool.live_config("cache", Cache::default()).await.unwrap();

        env.exec("UPDATE cfg_config SET value = '7' WHERE namespace = 'cache'")
            .await;
        eventually(5, || async { cache.get().ttl_secs == 7 }).await;
    }

    /// SeaORM 连接池：动态配置跟着它的 search_path，表建在那个 schema 下
    #[tokio::test]
    async fn seaorm_pool_follows_search_path() {
        let env = Env::new().await;
        env.exec("CREATE SCHEMA app").await;
        let config = SeaOrmPgConfig {
            connect: PgConnectConfig {
                schema_search_path: "app".to_string(),
                ..connect(&env.server)
            },
            live_config: live_config(),
            ..Default::default()
        };
        let pool = SeaOrmPgPool::from_config(&config, &app()).await.unwrap();
        let cache = pool.live_config("cache", Cache::default()).await.unwrap();

        let created: bool = sqlx::query_scalar("SELECT to_regclass('app.hy_config') IS NOT NULL")
            .fetch_one(&*env.pool)
            .await
            .unwrap();
        assert!(created);
        env.exec("UPDATE app.hy_config SET value = '8' WHERE namespace = 'cache'")
            .await;
        eventually(5, || async { cache.get().ttl_secs == 8 }).await;
        pool.close().await.unwrap();
    }

    /// 监听连接被服务端断掉：重连后对一遍版本，断线期间的修改也能拿到
    #[tokio::test]
    async fn reconnects_after_connection_killed() {
        let env = Env::new().await;
        let source = env.start().await;
        let cache = source.live_config("cache", Cache::default()).await.unwrap();

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
}
