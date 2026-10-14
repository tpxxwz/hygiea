//! `SqlxSqlitePool`（`src/sqlx/pool/sqlite.rs`，feature `sqlx` + `sqlite`）的集成测试，feature 全开（`live-config`）。SQLite 嵌在进程里，
//! 不需要 Docker：内存库（`:memory:`）和本地临时文件两种都测，默认的 `cargo test` 就会跑：
//! ```sh
//! cargo test -p hygiea-db --features sqlx,sqlite,live-config --test sqlx_sqlite
//! ```

#![cfg(all(feature = "sqlx", feature = "sqlite", feature = "live-config"))]

mod common;

use std::time::Duration;

use common::eventually;
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

/// 连接池本身：内存库、本地文件都能连上、执行，经 Registry 启动和停止
mod pool {
    use hygiea_core::app::{Component, Name, Registry, Resources};
    use hygiea_db::{
        DbResources, PoolConfig, SqliteConnectConfig, SqlxSqliteComponent, SqlxSqliteConfig,
        SqlxSqlitePool,
    };

    fn memory_config() -> SqlxSqliteConfig {
        SqlxSqliteConfig {
            connect: SqliteConnectConfig {
                database: ":memory:".to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
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

    /// 没启动过就 stop，以及 stop 两次，都是空操作
    #[tokio::test]
    async fn stop_without_startup_is_noop() {
        let mut component = SqlxSqliteComponent::build(Name::from("primary"), memory_config());
        component.stop().await.unwrap();
        component.stop().await.unwrap();
    }

    /// 匿名添加：`get::<SqlxSqlitePool>()` 取到能用的连接池
    #[tokio::test]
    async fn anonymous_component_provides_pool() {
        let registry = Registry::new().add::<SqlxSqliteComponent>(memory_config());
        let pool = with_resources(registry, |res| res.get::<SqlxSqlitePool>())
            .await
            .unwrap();
        let value: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(value, 1);
    }

    /// 具名添加多个实例：按名字各取各的，匿名取不到
    #[tokio::test]
    async fn named_components_provide_pools_by_name() {
        let registry = Registry::new()
            .add_named::<SqlxSqliteComponent>("primary", memory_config())
            .add_named::<SqlxSqliteComponent>("replica", memory_config());
        let (primary, replica, anonymous) = with_resources(registry, |res| {
            (
                res.get_named::<SqlxSqlitePool>("primary"),
                res.get_named::<SqlxSqlitePool>("replica"),
                res.get::<SqlxSqlitePool>().is_some(),
            )
        })
        .await;
        for pool in [primary.unwrap(), replica.unwrap()] {
            let value: i32 = sqlx::query_scalar("SELECT 1")
                .fetch_one(&*pool)
                .await
                .unwrap();
            assert_eq!(value, 1);
        }
        assert!(!anonymous);
    }

    /// 优雅关闭时组件的 stop 关掉连接池。要给自己发 SIGTERM 触发关闭，只在 unix 上跑
    #[cfg(unix)]
    #[tokio::test]
    async fn graceful_shutdown_closes_pool() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let run = Registry::new()
            .add_named::<SqlxSqliteComponent>("primary", memory_config())
            .on_ready(move |res| async move {
                let _ = tx.send(res.get_named::<SqlxSqlitePool>("primary"));
                Ok(())
            })
            .run();

        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            // 用系统的 kill 命令给自己发 SIGTERM，不额外引入依赖
            std::process::Command::new("kill")
                .args(["-TERM", &std::process::id().to_string()])
                .status()
                .unwrap();
        });

        let (run_result, _log_guard) = run.await;
        assert!(run_result.is_ok(), "{run_result:?}");

        let pool = rx.await.unwrap().unwrap();
        let err = sqlx::query("SELECT 1").fetch_one(&*pool).await.unwrap_err();
        assert!(matches!(err, sqlx::Error::PoolClosed), "{err:?}");
    }

    /// 内存库和本地文件：连上、建表、在事务里写入再读回
    #[tokio::test]
    async fn memory_and_file_databases_work() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pool.db").display().to_string();
        for database in [":memory:".to_string(), file] {
            // 内存库（不共享缓存）每条连接各是一个库，池限成 1 条连接
            let config = SqlxSqliteConfig {
                connect: SqliteConnectConfig {
                    database: database.clone(),
                    ..Default::default()
                },
                pool: Some(PoolConfig {
                    max_connections: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let db = DbResources::from_config(&config, &Default::default())
                .await
                .unwrap();
            let pool = db.pool.clone().unwrap();
            hygiea_db::sqlx::transaction(&*pool, "create and insert", async |tx| {
                sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT NOT NULL)")
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                sqlx::query("INSERT INTO t (id, v) VALUES (1, 'a')")
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                Ok(())
            })
            .await
            .unwrap();
            let v: String = sqlx::query_scalar("SELECT v FROM t WHERE id = 1")
                .fetch_one(&*pool)
                .await
                .unwrap();
            assert_eq!(v, "a", "{database}");
            db.close().await.unwrap();
        }
    }
}

/// 动态配置（feature `live-config`）：小池另开一条连接，内存库连到的是另一个空库，所以用本地临时文件
mod live_config {
    use hygiea_core::app::RegistryConfig;
    use hygiea_db::{
        DbResources, PoolConfig, RepoErr, SqliteConnectConfig, SqliteConnectOverride,
        SqliteLiveConfigConfig, SqlxSqliteConfig, SqlxSqlitePool,
    };
    use tempfile::TempDir;

    use super::common::{LiveInst, open_pool, uses};
    use super::*;

    type Inst = LiveInst<SqlxSqliteConfig>;

    /// 应用名 `hy`：表是 hy_config、hy_config_revision、hy_config_history
    fn app() -> RegistryConfig {
        RegistryConfig {
            app_name: "hy".to_string(),
            ..Default::default()
        }
    }

    /// 一个临时库。业务池模拟别的程序改表，动态配置走它自己的那条连接
    struct Env {
        _dir: TempDir,
        /// `uses = 3`：业务池加动态配置
        config: SqlxSqliteConfig,
        source: Inst,
        pool: SqlxSqlitePool,
    }

    impl Env {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let config = SqlxSqliteConfig {
                uses: uses(3),
                connect: SqliteConnectConfig {
                    database: dir.path().join("dyn.db").display().to_string(),
                    ..Default::default()
                },
                live_config: Some(SqliteLiveConfigConfig::default()),
                ..Default::default()
            };
            let source = Inst::open(&config, &app()).await;
            let pool = source.pool().clone();
            Self {
                _dir: dir,
                config,
                source,
                pool,
            }
        }

        /// 同一个库再建一次，相当于重启程序
        async fn restart(&self) -> Inst {
            Inst::open(&self.config, &app()).await
        }

        /// 先绑一个空的 namespace：第一次 bind 才建表，测试要在 bind 前往表里写数据
        async fn start(&self) -> Inst {
            let source = self.source.clone();
            source.bind("_start", Empty {}).await.unwrap();
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
        let sync = source.bind("sync", Job::default()).await.unwrap();

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

        let sync = source.bind("sync", Job::default()).await.unwrap();
        assert!(sync.get().enabled);
        assert_eq!(env.value("sync", "enabled").await.as_deref(), Some("true"));
    }

    /// 别的连接改值：轮询发现，changed 返回，get 拿到新值；history 记 U 带新旧值
    #[tokio::test]
    async fn update_is_picked_up_by_polling() {
        let env = Env::new().await;
        let source = env.start().await;
        let mut sync = source.bind("sync", Job::default()).await.unwrap();

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
        source.bind("sync", Job::default()).await.unwrap();

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
        let sync = source.bind("sync", Job::default()).await.unwrap();
        assert_eq!(sync.get().start_time, "02:00");

        env.exec("DELETE FROM hy_config WHERE namespace = 'sync' AND key = 'start_time'")
            .await;
        eventually(5, || async { sync.get().start_time == "01:00" }).await;
    }

    async fn history_count(pool: &SqlxSqlitePool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM hy_config_history")
            .fetch_one(&**pool)
            .await
            .unwrap()
    }

    /// 配了保留行数：后台任务一启动就删到只剩最新的 N 行
    #[tokio::test]
    async fn history_keeps_latest_rows() {
        let env = Env::new().await;
        let source = env.start().await;
        let _sync = source.bind("sync", Job::default()).await.unwrap();
        env.exec("UPDATE hy_config SET value = '[3]' WHERE namespace = 'sync' AND key = 'levels'")
            .await;
        assert_eq!(history_count(&env.pool).await, 4);

        let mut config = env.config.clone();
        config.live_config.as_mut().unwrap().history_keep_rows = Some(2);
        config
            .live_config
            .as_mut()
            .unwrap()
            .history_trim_interval_mins = Some(60);
        let restarted = Inst::open(&config, &app()).await;
        let _sync = restarted.bind("sync", Job::default()).await.unwrap();
        eventually(5, || async { history_count(&env.pool).await == 2 }).await;
        // 留下的是最新的：最后一条是刚才的 U
        let op: String =
            sqlx::query_scalar("SELECT op FROM hy_config_history ORDER BY id DESC LIMIT 1")
                .fetch_one(&*env.pool)
                .await
                .unwrap();
        assert_eq!(op, "U");
    }

    /// 保留行数、清理间隔要么都配要么都不配，配了都要是正数
    #[tokio::test]
    async fn history_options_are_checked() {
        // 库放在临时目录：默认的库路径是相对当前目录的文件
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("options.db").display().to_string();
        let open = |rows: Option<u64>, mins: Option<u64>| {
            let database = database.clone();
            async move {
                let mut config = SqlxSqliteConfig {
                    uses: uses(3),
                    live_config: Some(SqliteLiveConfigConfig {
                        history_keep_rows: rows,
                        history_trim_interval_mins: mins,
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                config.connect.database = database;
                DbResources::from_config(&config, &app()).await
            }
        };
        use hygiea_core::BaseErr::NotPositive;
        use hygiea_core::app::AppErr::InvalidConfig;
        assert!(open(Some(10), None).await.unwrap_err().is(InvalidConfig));
        assert!(open(None, Some(60)).await.unwrap_err().is(InvalidConfig));
        assert!(open(Some(0), Some(60)).await.unwrap_err().is(NotPositive));
        assert!(open(Some(10), Some(0)).await.unwrap_err().is(NotPositive));
        assert!(open(None, None).await.is_ok());
        assert!(open(Some(10), Some(60)).await.is_ok());
    }

    /// 改成转不了的值：保留旧值；改回合法的值后恢复更新
    #[tokio::test]
    async fn bad_value_keeps_old() {
        let env = Env::new().await;
        let source = env.start().await;
        let sync = source.bind("sync", Job::default()).await.unwrap();

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

        let e = source.bind("sync", Job::default()).await.unwrap_err();
        assert!(e.is(RepoErr::LiveConfigDecodeFailed), "{e:#}");
    }

    /// 默认值不是 JSON 对象：DefaultsInvalid
    #[tokio::test]
    async fn defaults_must_be_object() {
        let env = Env::new().await;
        let source = env.start().await;
        let e = source.bind("list", vec![1, 2]).await.unwrap_err();
        assert!(e.is(RepoErr::LiveConfigDefaultsInvalid), "{e:#}");

        #[derive(Debug, Serialize, Deserialize)]
        struct Root {
            flag: bool,
        }
        let e = source.bind_all(Root { flag: true }).await.unwrap_err();
        assert!(e.is(RepoErr::LiveConfigDefaultsInvalid), "{e:#}");
    }

    /// bind_all：第一层是 namespace；只改一个 namespace，另一个的值不变
    #[tokio::test]
    async fn bind_all_namespaces() {
        let env = Env::new().await;
        let source = env.start().await;
        let all = source.bind_all(All::default()).await.unwrap();
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
        let all = source.bind_all(All::default()).await.unwrap();
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
        source.bind("cache", V1 { ttl_secs: 60 }).await.unwrap();
        env.exec(
            "UPDATE hy_config SET value = '90' WHERE namespace = 'cache' AND key = 'ttl_secs'",
        )
        .await;

        let v2 = source
            .bind(
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
        source.bind("cache", Cache::default()).await.unwrap();

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
        started.source.bind("sync", Job::default()).await.unwrap();
        started.source.close().await.unwrap();
        let e = started
            .source
            .bind("sync", Job::default())
            .await
            .unwrap_err();
        assert!(e.is(RepoErr::LiveConfigClosed), "{e:#}");

        let never_started = Env::new().await;
        never_started.source.close().await.unwrap();
        let e = never_started
            .source
            .bind("sync", Job::default())
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
            .source
            .bind("cache", Cache::default())
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
            .bind("cache", Cache::default())
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
            .source
            .bind("cache", Cache::default())
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
        source.bind("cache", Cache::default()).await.unwrap();
        env.exec("UPDATE hy_config SET value = '99' WHERE namespace = 'cache'")
            .await;
        env.exec("DROP TRIGGER hy_config_delete").await;

        let e = env
            .restart()
            .await
            .bind("cache", Cache::default())
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
            uses: uses(3),
            connect: SqliteConnectConfig {
                database: dir.path().join("dedicated.db").display().to_string(),
                ..Default::default()
            },
            pool: Some(PoolConfig {
                max_connections: Some(1),
                acquire_timeout_secs: Some(1),
                ..Default::default()
            }),
            live_config: Some(SqliteLiveConfigConfig::default()),
        };
        let pool = Inst::open(&config, &app()).await;
        let _held = pool.pool().acquire().await.unwrap();

        let cache = pool.bind("cache", Cache::default()).await.unwrap();
        assert_eq!(*cache.get(), Cache::default());
    }

    /// `uses` 不带 2：没有动态配置，也不建表；写了 `[live_config]` 段启动报错
    #[tokio::test]
    async fn not_in_uses() {
        let dir = tempfile::tempdir().unwrap();
        let config = SqlxSqliteConfig {
            connect: SqliteConnectConfig {
                database: dir.path().join("off.db").display().to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db = DbResources::from_config(&config, &app()).await.unwrap();
        assert!(db.live_config.is_none());
        let tables: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name LIKE 'hy_config%'")
                .fetch_one(&**db.pool.as_ref().unwrap())
                .await
                .unwrap();
        assert_eq!(tables, 0);

        let written = SqlxSqliteConfig {
            live_config: Some(SqliteLiveConfigConfig::default()),
            ..config
        };
        let e = DbResources::from_config(&written, &app())
            .await
            .unwrap_err();
        assert!(e.is(hygiea_core::app::AppErr::InvalidConfig), "{e:#}");
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

    /// `[db.live_config.connect]` 换个库文件、`table_prefix` 写空的：表建在那个库里、不带前缀，主库里没有
    #[tokio::test]
    async fn own_database_and_empty_prefix() {
        let env = Env::new().await;
        let other = env._dir.path().join("config.db").display().to_string();
        let mut config = env.config.clone();
        config.live_config.as_mut().unwrap().table_prefix = Some(String::new());
        config.live_config.as_mut().unwrap().connect = SqliteConnectOverride {
            database: Some(other.clone()),
            ..Default::default()
        };
        let pool = Inst::open(&config, &app()).await;
        let cache = pool.bind("cache", Cache::default()).await.unwrap();

        let count = |pool: SqlxSqlitePool| async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name LIKE '%config%'",
            )
            .fetch_one(&*pool)
            .await
            .unwrap()
        };
        assert_eq!(count(pool.pool().clone()).await, 0, "主库里没有");
        let config_db = open_pool(
            &SqlxSqliteConfig {
                connect: SqliteConnectConfig {
                    database: other,
                    ..Default::default()
                },
                ..Default::default()
            },
            &app(),
        )
        .await;
        assert_eq!(count(config_db.clone()).await, 3);
        sqlx::query("UPDATE config SET value = '5' WHERE namespace = 'cache'")
            .execute(&*config_db)
            .await
            .unwrap();
        eventually(5, || async { cache.get().ttl_secs == 5 }).await;
    }
}
