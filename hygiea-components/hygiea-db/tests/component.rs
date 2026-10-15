//! `DbComponent`（`src/component.rs`）的综合测试：一个 Registry 里 6 种连接池组件、7 个组件一起启动，
//! 各自不同的 `uses`、数据库、用户、schema。配置写成 TOML（`[db.<名字>]`）再解析，和应用里的写法一样。
//! container 层，PG、MySQL 都用代码启动容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）：
//! ```sh
//! cargo test -p hygiea-db --features sqlx,seaorm,postgres,mysql,sqlite,lock-row,live-config --test component -- --ignored ::container::
//! ```
//!
//! | 组件 | 类型 | `uses` | 连到哪 |
//! |---|---|---|---|
//! | `main` | SeaORM PG | 3 业务池 + 动态配置 | 库 `app`、用户 `app_user`、schema `app`；动态配置换成用户 `cfg_user`、schema `cfg`（覆盖段） |
//! | `replica` | sqlx PG | 1 业务池（不写 `uses`） | 库 `app`、只读用户 `app_ro` |
//! | `platform` | sqlx PG | 2 只有动态配置 | 另一个库 `platform`、用户 `platform_user`、表名前缀 `platform` |
//! | `legacy` | sqlx MySQL | 5 业务池 + 锁 | 库 `legacy`、用户 `legacy_user`；锁换成用户 `locker`、单独建池（覆盖段） |
//! | `shop` | SeaORM MySQL | 3 业务池 + 动态配置 | 库 `shop`、用户 `shop_user` |
//! | `cache` | sqlx SQLite | 1 业务池 | 本地文件 `cache.db` |
//! | `local` | SeaORM SQLite | 2 只有动态配置 | 本地文件 `local.db` |
//!
//! 每个测试自己起一套容器：
//! - `resources_follow_uses`：Resources 里放了什么、各连哪个库哪个用户、动态配置的表建在哪；锁全应用只能有一个
//! - `crud_on_every_pool`：5 个业务池各走一遍增删改查和事务，从库只读
//! - `live_config_every_source`：4 个动态配置源都绑同一个 namespace，各改各的表，改值、加删 key、转不了的值、历史
//! - `lock_between_two_instances`：再起一个 Registry 当第二个实例，两边的锁互斥：三种 key、try 和阻塞版、drop 释放、参数检查

#![cfg(all(
    feature = "sqlx",
    feature = "seaorm",
    feature = "postgres",
    feature = "mysql",
    feature = "sqlite",
    feature = "lock-row",
    feature = "live-config"
))]

mod common;

use hygiea_test::container;

#[container]
mod registry {
    use std::time::Duration;

    use hygiea_core::app::{AppErr, Registry, RegistryConfig, Resources};
    use hygiea_core::lock::{DistributedLock, LockErr, hy_lock_key, hy_lock_type, lock_key};
    use hygiea_core::{ResultExt, err};
    use hygiea_db::{
        DbLiveConfig, DbLockClient, DbUses, RepoErr, SeaOrmMySqlComponent, SeaOrmMySqlConfig,
        SeaOrmMySqlPool, SeaOrmPgComponent, SeaOrmPgConfig, SeaOrmPgPool, SeaOrmSqliteComponent,
        SeaOrmSqliteConfig, SeaOrmSqlitePool, SqlxMySqlComponent, SqlxMySqlConfig, SqlxMySqlPool,
        SqlxPgComponent, SqlxPgConfig, SqlxPgPool, SqlxSqliteComponent, SqlxSqliteConfig,
        SqlxSqlitePool,
    };
    use hygiea_test::container::{ContainerSpec, RunningContainer};
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
    use serde::{Deserialize, Serialize};

    use super::common::eventually;

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

    // ---- 配置 -----------------------------------------------------------------------

    /// 应用自己的配置结构体：所有库放在 `[db]` 下，字段名就是组件名，字段类型决定是哪种连接池
    #[derive(Deserialize)]
    struct AppConfig {
        db: Dbs,
    }

    #[derive(Deserialize)]
    struct Dbs {
        main: SeaOrmPgConfig,
        replica: SqlxPgConfig,
        platform: SqlxPgConfig,
        legacy: SqlxMySqlConfig,
        shop: SeaOrmMySqlConfig,
        cache: SqlxSqliteConfig,
        local: SeaOrmSqliteConfig,
    }

    const PG_PASSWORD: &str = "pg-root";
    const PG_PORT: u16 = 5432;
    const MYSQL_PASSWORD: &str = "mysql-root";
    const MYSQL_PORT: u16 = 3306;

    /// 应用的配置文件：所有库放在 `[db.<名字>]` 下
    fn config_toml(
        pg: &RunningContainer,
        mysql: &RunningContainer,
        dir: &tempfile::TempDir,
    ) -> String {
        let (pg_host, pg_port) = (pg.host(), pg.port(PG_PORT));
        let (mysql_host, mysql_port) = (mysql.host(), mysql.port(MYSQL_PORT));
        let cache = dir.path().join("cache.db").display().to_string();
        let local = dir.path().join("local.db").display().to_string();
        format!(
            r#"
# PG 主库（SeaORM）：业务池 + 动态配置。动态配置在同一个库，换用户和 schema
[db.main]
uses = 3
[db.main.connect]
host = "{pg_host}"
port = {pg_port}
username = "app_user"
password = "app-pw"
database = "app"
schema_search_path = "app"
[db.main.log]
level = "debug"
[db.main.live_config]
poll_interval_secs = 3600
[db.main.live_config.connect]
username = "cfg_user"
password = "cfg-pw"
schema_search_path = "cfg"

# PG 从库：只当业务池（不写 uses 就是 1），只读用户
[db.replica.connect]
host = "{pg_host}"
port = {pg_port}
username = "app_ro"
password = "ro-pw"
database = "app"
schema_search_path = "app"
[db.replica.pool]
max_connections = 2

# PG 平台公共配置库：只给动态配置用，另一个库、另一个用户、自己的表名前缀
[db.platform]
uses = 2
[db.platform.connect]
host = "{pg_host}"
port = {pg_port}
username = "platform_user"
password = "platform-pw"
database = "platform"
[db.platform.live_config]
table_prefix = "platform"
poll_interval_secs = 3600

# MySQL 老系统库：业务池 + 锁。锁换成 locker 用户、单独建池
[db.legacy]
uses = 5
[db.legacy.connect]
host = "{mysql_host}"
port = {mysql_port}
username = "legacy_user"
password = "legacy-pw"
database = "legacy"
[db.legacy.lock]
max_connections = 4
[db.legacy.lock.connect]
username = "locker"
password = "locker-pw"

# MySQL 商城库（SeaORM）：业务池 + 动态配置（只靠轮询）
[db.shop]
uses = 3
[db.shop.connect]
host = "{mysql_host}"
port = {mysql_port}
username = "shop_user"
password = "shop-pw"
database = "shop"
[db.shop.live_config]
poll_interval_secs = 1

# SQLite 本地缓存库：只当业务池
[db.cache.connect]
database = "{cache}"

# SQLite 本地配置（SeaORM）：只给动态配置用
[db.local]
uses = 2
[db.local.connect]
database = "{local}"
"#
        )
    }

    // ---- 环境：两个容器、建库建用户、启动 Registry ---------------------------------------------

    async fn start_pg() -> RunningContainer {
        ContainerSpec::new("postgres", "18")
            .env("POSTGRES_PASSWORD", PG_PASSWORD)
            .port(PG_PORT)
            .wait_log_times("database system is ready to accept connections", 2)
            .start()
            .await
    }

    /// 启动时先起临时实例做初始化再重启，就绪日志出现两次
    async fn start_mysql() -> RunningContainer {
        ContainerSpec::new("mysql", "8.4")
            .env("MYSQL_ROOT_PASSWORD", MYSQL_PASSWORD)
            .port(MYSQL_PORT)
            .wait_log_times("mysqld: ready for connections", 2)
            .startup_timeout(Duration::from_secs(180))
            .start()
            .await
    }

    async fn pg_exec(pool: &sqlx::PgPool, sql: &str) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(pool)
            .await
            .unwrap();
    }

    async fn mysql_exec(pool: &sqlx::MySqlPool, sql: &str) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(pool)
            .await
            .unwrap();
    }

    /// 一个跑着的 Registry（一个应用实例），drop 时停掉
    struct Running {
        res: Resources,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// 启动 registry，在 on_ready 里把 Resources 交出来；`run` 之后一直等退出信号
    async fn run(registry: Registry) -> Running {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (result, _log_guard) = registry
                .on_ready(move |resources| async move {
                    let _ = tx.send(resources);
                    Ok(())
                })
                .run()
                .await;
            result.unwrap();
        });
        Running {
            res: rx.await.unwrap(),
            task,
        }
    }

    /// 两个容器、临时目录，加上按配置起好的第一个实例
    struct Env {
        pg: RunningContainer,
        mysql: RunningContainer,
        dir: tempfile::TempDir,
        app: Option<Running>,
    }

    impl Env {
        async fn new() -> Self {
            let (pg, mysql) = tokio::join!(start_pg(), start_mysql());
            let mut env = Self {
                pg,
                mysql,
                dir: tempfile::tempdir().unwrap(),
                app: None,
            };
            tokio::join!(env.setup_pg(), env.setup_mysql());
            env.app = Some(env.instance().await);
            env
        }

        /// PG：四个用户；库 `app`（schema `app` 给业务、`cfg` 给动态配置，只读用户能用 `app`）和库 `platform`
        async fn setup_pg(&self) {
            let admin = self.pg_admin("postgres").await;
            pg_exec(
                &admin,
                "CREATE ROLE app_user LOGIN PASSWORD 'app-pw'; \
                 CREATE ROLE app_ro LOGIN PASSWORD 'ro-pw'; \
                 CREATE ROLE cfg_user LOGIN PASSWORD 'cfg-pw'; \
                 CREATE ROLE platform_user LOGIN PASSWORD 'platform-pw';",
            )
            .await;
            // CREATE DATABASE 不能放进一个多语句（隐式事务）里，一条一条执行
            pg_exec(&admin, "CREATE DATABASE app OWNER app_user").await;
            pg_exec(&admin, "CREATE DATABASE platform OWNER platform_user").await;
            pg_exec(
                &self.pg_admin("app").await,
                "CREATE SCHEMA app AUTHORIZATION app_user; \
                 CREATE SCHEMA cfg AUTHORIZATION cfg_user; \
                 GRANT USAGE ON SCHEMA app TO app_ro;",
            )
            .await;
        }

        /// MySQL：库 `legacy`、`shop`；`locker` 只给锁用（GET_LOCK 不要权限，连库要有一点）
        async fn setup_mysql(&self) {
            mysql_exec(
                &self.mysql_root().await,
                // MySQL 默认开着 binlog，没有 SUPER 的用户建触发器报 1419，要打开这个开关（见 live_config 模块文档）
                "SET GLOBAL log_bin_trust_function_creators = 1; \
                 CREATE DATABASE legacy; \
                 CREATE DATABASE shop; \
                 CREATE USER 'legacy_user'@'%' IDENTIFIED BY 'legacy-pw'; \
                 GRANT ALL ON legacy.* TO 'legacy_user'@'%'; \
                 CREATE USER 'locker'@'%' IDENTIFIED BY 'locker-pw'; \
                 GRANT SELECT ON legacy.* TO 'locker'@'%'; \
                 CREATE USER 'shop_user'@'%' IDENTIFIED BY 'shop-pw'; \
                 GRANT ALL ON shop.* TO 'shop_user'@'%';",
            )
            .await;
        }

        fn dbs(&self) -> Dbs {
            toml::from_str::<AppConfig>(&config_toml(&self.pg, &self.mysql, &self.dir))
                .unwrap()
                .db
        }

        /// 按配置起一个应用实例（应用名 `hy`）：7 个组件，名字和字段名一样
        async fn instance(&self) -> Running {
            let c = self.dbs();
            let registry = Registry::with_config(RegistryConfig {
                app_name: "hy".to_string(),
                ..Default::default()
            })
            .add_named::<SeaOrmPgComponent>("main", c.main)
            .add_named::<SqlxPgComponent>("replica", c.replica)
            .add_named::<SqlxPgComponent>("platform", c.platform)
            .add_named::<SqlxMySqlComponent>("legacy", c.legacy)
            .add_named::<SeaOrmMySqlComponent>("shop", c.shop)
            .add_named::<SqlxSqliteComponent>("cache", c.cache)
            .add_named::<SeaOrmSqliteComponent>("local", c.local);
            run(registry).await
        }

        /// 第一个实例的 Resources
        fn res(&self) -> &Resources {
            &self.app.as_ref().unwrap().res
        }

        /// 超级用户连 PG 的某个库：建用户、库、schema，查表在哪，改动态配置的表（模拟别的程序）
        async fn pg_admin(&self, database: &str) -> sqlx::PgPool {
            let url = format!(
                "postgres://postgres:{PG_PASSWORD}@{}:{}/{database}",
                self.pg.host(),
                self.pg.port(PG_PORT)
            );
            sqlx::PgPool::connect(&url).await.unwrap()
        }

        async fn mysql_root(&self) -> sqlx::MySqlPool {
            let url = format!(
                "mysql://root:{MYSQL_PASSWORD}@{}:{}/",
                self.mysql.host(),
                self.mysql.port(MYSQL_PORT)
            );
            sqlx::MySqlPool::connect(&url).await.unwrap()
        }

        /// 直接打开 `local` 组件的 SQLite 文件
        async fn local_sqlite(&self) -> sqlx::SqlitePool {
            let path = self.dir.path().join("local.db");
            sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap()
        }
    }

    // ---- 1. Resources 跟着 uses 走 -------------------------------------------------------

    fn pg_sql(sql: &str) -> Statement {
        Statement::from_string(sea_orm::DbBackend::Postgres, sql)
    }

    fn mysql_sql(sql: &str) -> Statement {
        Statement::from_string(sea_orm::DbBackend::MySql, sql)
    }

    /// SeaORM 连接上查一行两列字符串
    async fn two_strings(db: &DatabaseConnection, sql: Statement) -> (String, String) {
        let row = db.query_one_raw(sql).await.unwrap().unwrap();
        (
            row.try_get_by_index(0).unwrap(),
            row.try_get_by_index(1).unwrap(),
        )
    }

    #[derive(Debug, Default, Serialize, Deserialize)]
    struct Empty {}

    /// 业务池、动态配置按组件名，锁匿名；没开的位什么都不放。各个池连的库和用户、动态配置连的用户和表的位置都对；
    /// 再加一个也开锁的组件（另一种数据库也算）时启动前报 DuplicateProvider
    #[tokio::test]
    async fn resources_follow_uses() {
        let env = Env::new().await;
        let res = env.res();

        let main = res.get_named::<SeaOrmPgPool>("main").unwrap();
        let replica = res.get_named::<SqlxPgPool>("replica").unwrap();
        let legacy = res.get_named::<SqlxMySqlPool>("legacy").unwrap();
        let shop = res.get_named::<SeaOrmMySqlPool>("shop").unwrap();
        let cache = res.get_named::<SqlxSqlitePool>("cache").unwrap();
        assert!(
            !res.contains_named::<SqlxPgPool>("platform"),
            "uses = 2 没有业务池"
        );
        assert!(!res.contains_named::<SeaOrmSqlitePool>("local"));
        for name in ["main", "platform", "shop", "local"] {
            assert!(
                res.contains_named::<DbLiveConfig>(name),
                "{name} 有动态配置"
            );
        }
        for name in ["replica", "legacy", "cache"] {
            assert!(
                !res.contains_named::<DbLiveConfig>(name),
                "{name} 没有动态配置"
            );
        }
        assert!(res.contains::<DbLockClient>());

        // 业务池：各连各的库、各用各的用户
        assert_eq!(
            two_strings(
                &main,
                pg_sql("SELECT current_user::text, current_schema()::text")
            )
            .await,
            ("app_user".to_string(), "app".to_string())
        );
        let who: (String, String) =
            sqlx::query_as("SELECT current_user::text, current_schema()::text")
                .fetch_one(&*replica)
                .await
                .unwrap();
        assert_eq!(who, ("app_ro".to_string(), "app".to_string()));
        let who: (String, String) = sqlx::query_as("SELECT CURRENT_USER(), DATABASE()")
            .fetch_one(&*legacy)
            .await
            .unwrap();
        assert_eq!(who, ("legacy_user@%".to_string(), "legacy".to_string()));
        assert_eq!(
            two_strings(&shop, mysql_sql("SELECT CURRENT_USER(), DATABASE()")).await,
            ("shop_user@%".to_string(), "shop".to_string())
        );
        let file: String =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name = 'main'")
                .fetch_one(&*cache)
                .await
                .unwrap();
        assert!(file.ends_with("cache.db"), "{file}");

        // 动态配置第一次绑定才连库、建表
        for name in ["main", "platform", "shop", "local"] {
            let source = res.get_named::<DbLiveConfig>(name).unwrap();
            source.bind("_start", Empty {}).await.unwrap();
        }
        // main 的源在库 app 用 cfg_user 连、表建在 schema cfg；platform 的源在库 platform 用 platform_user 连
        let sources: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT datname::text, usename::text FROM pg_stat_activity \
             WHERE application_name = 'hy-live-config' ORDER BY 1",
        )
        .fetch_all(&env.pg_admin("postgres").await)
        .await
        .unwrap();
        assert_eq!(
            sources,
            vec![
                ("app".to_string(), "cfg_user".to_string()),
                ("platform".to_string(), "platform_user".to_string()),
            ]
        );
        let (owner, in_app): (String, bool) = sqlx::query_as(
            "SELECT tableowner::text, to_regclass('app.hy_config') IS NOT NULL \
             FROM pg_tables WHERE schemaname = 'cfg' AND tablename = 'hy_config'",
        )
        .fetch_one(&env.pg_admin("app").await)
        .await
        .unwrap();
        assert_eq!(
            (owner.as_str(), in_app),
            ("cfg_user", false),
            "不在业务的 schema 里"
        );
        let owner: String = sqlx::query_scalar(
            "SELECT tableowner::text FROM pg_tables WHERE tablename = 'platform_config'",
        )
        .fetch_one(&env.pg_admin("platform").await)
        .await
        .unwrap();
        assert_eq!(owner, "platform_user");
        let schemas: Vec<String> = sqlx::query_scalar(
            "SELECT table_schema FROM information_schema.tables WHERE table_name = 'hy_config'",
        )
        .fetch_all(&env.mysql_root().await)
        .await
        .unwrap();
        assert_eq!(schemas, vec!["shop"], "MySQL 只有 shop 库里有");
        let tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name LIKE 'hy_config%'",
        )
        .fetch_one(&env.local_sqlite().await)
        .await
        .unwrap();
        assert_eq!(tables, 3);

        // 锁全应用只能有一个，不管哪种数据库：PG 再开一个只给锁用的组件，启动前报错
        let c = env.dbs();
        let pg_lock = SqlxPgConfig {
            uses: DbUses::try_from(DbUses::LOCK).unwrap(),
            connect: c.replica.connect.clone(),
            ..Default::default()
        };
        let (result, _log_guard) = Registry::new()
            .add_named::<SqlxMySqlComponent>("legacy", c.legacy)
            .add_named::<SqlxPgComponent>("coord", pg_lock)
            .run()
            .await;
        let e = result.unwrap_err();
        assert!(e.is(AppErr::DuplicateProvider), "{e:#}");
    }

    // ---- 2. 增删改查 ---------------------------------------------------------------------

    /// 各数据库都认的写法：建表、插两行、查、改一行、删一行；事务提交一次、回滚一次（第二条撞主键，整个撤掉）
    macro_rules! sqlx_crud {
        ($pool:expr) => {{
            let pool = $pool;
            let exec = async |sql: &'static str| {
                sqlx::raw_sql(sql).execute(pool).await.unwrap();
            };
            let names = async || -> Vec<String> {
                sqlx::query_scalar("SELECT name FROM item ORDER BY id")
                    .fetch_all(pool)
                    .await
                    .unwrap()
            };
            exec("CREATE TABLE item (id INT PRIMARY KEY, name VARCHAR(32) NOT NULL)").await;
            exec("INSERT INTO item (id, name) VALUES (1, 'a'), (2, 'b')").await;
            assert_eq!(names().await, ["a", "b"]);
            exec("UPDATE item SET name = 'A' WHERE id = 1").await;
            exec("DELETE FROM item WHERE id = 2").await;
            assert_eq!(names().await, ["A"]);

            hygiea_db::sqlx::transaction(pool, "insert c", async |tx| {
                sqlx::raw_sql("INSERT INTO item (id, name) VALUES (3, 'c')")
                    .execute(&mut **tx)
                    .await
                    .wrap_err(|| err!(RepoErr::InsertFailed, "insert c"))?;
                Ok(())
            })
            .await
            .unwrap();
            let e = hygiea_db::sqlx::transaction(pool, "insert d, dup", async |tx| {
                for sql in [
                    "INSERT INTO item (id, name) VALUES (4, 'd')",
                    "INSERT INTO item (id, name) VALUES (1, 'dup')",
                ] {
                    sqlx::raw_sql(sql)
                        .execute(&mut **tx)
                        .await
                        .wrap_err(|| err!(RepoErr::InsertFailed, "insert"))?;
                }
                Ok(())
            })
            .await
            .unwrap_err();
            assert!(e.is(RepoErr::InsertFailed), "{e:#}");
            assert_eq!(names().await, ["A", "c"], "回滚了，d 没留下");
        }};
    }

    /// SeaORM 版，内容和 `sqlx_crud!` 一样
    async fn seaorm_crud(db: &DatabaseConnection) {
        let exec = async |sql: &str| {
            db.execute_unprepared(sql).await.unwrap();
        };
        let names = async || -> Vec<String> {
            db.query_all_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT name FROM item ORDER BY id",
            ))
            .await
            .unwrap()
            .iter()
            .map(|row| row.try_get_by_index(0).unwrap())
            .collect()
        };
        exec("CREATE TABLE item (id INT PRIMARY KEY, name VARCHAR(32) NOT NULL)").await;
        exec("INSERT INTO item (id, name) VALUES (1, 'a'), (2, 'b')").await;
        assert_eq!(names().await, ["a", "b"]);
        exec("UPDATE item SET name = 'A' WHERE id = 1").await;
        exec("DELETE FROM item WHERE id = 2").await;
        assert_eq!(names().await, ["A"]);

        hygiea_db::seaorm::transaction(db, "insert c", async |txn| {
            txn.execute_unprepared("INSERT INTO item (id, name) VALUES (3, 'c')")
                .await
                .wrap_err(|| err!(RepoErr::InsertFailed, "insert c"))?;
            Ok(())
        })
        .await
        .unwrap();
        let e = hygiea_db::seaorm::transaction(db, "insert d, dup", async |txn| {
            for sql in [
                "INSERT INTO item (id, name) VALUES (4, 'd')",
                "INSERT INTO item (id, name) VALUES (1, 'dup')",
            ] {
                txn.execute_unprepared(sql)
                    .await
                    .wrap_err(|| err!(RepoErr::InsertFailed, "insert"))?;
            }
            Ok(())
        })
        .await
        .unwrap_err();
        assert!(e.is(RepoErr::InsertFailed), "{e:#}");
        assert_eq!(names().await, ["A", "c"], "回滚了，d 没留下");
    }

    /// 5 个业务池各走一遍增删改查和事务；从库（只读用户）读得到主库写的，增删改都被拒绝
    #[tokio::test]
    async fn crud_on_every_pool() {
        let env = Env::new().await;
        let res = env.res();

        let main = res.get_named::<SeaOrmPgPool>("main").unwrap();
        seaorm_crud(&main).await;
        sqlx_crud!(&*res.get_named::<SqlxMySqlPool>("legacy").unwrap());
        seaorm_crud(&res.get_named::<SeaOrmMySqlPool>("shop").unwrap()).await;
        sqlx_crud!(&*res.get_named::<SqlxSqlitePool>("cache").unwrap());

        main.execute_unprepared("GRANT SELECT ON item TO app_ro")
            .await
            .unwrap();
        let replica = res.get_named::<SqlxPgPool>("replica").unwrap();
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM item ORDER BY id")
            .fetch_all(&*replica)
            .await
            .unwrap();
        assert_eq!(names, ["A", "c"], "从库读得到主库写的");
        for sql in [
            "INSERT INTO item (id, name) VALUES (9, 'x')",
            "UPDATE item SET name = 'x'",
            "DELETE FROM item",
        ] {
            let e = sqlx::raw_sql(sql).execute(&*replica).await.unwrap_err();
            assert!(e.to_string().contains("permission denied"), "{sql}: {e}");
        }
    }

    // ---- 3. 动态配置 ---------------------------------------------------------------------

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Feature {
        enabled: bool,
        level: u32,
        tags: Vec<String>,
    }

    impl Default for Feature {
        fn default() -> Self {
            Self {
                enabled: false,
                level: 1,
                tags: vec!["a".to_string()],
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Limits {
        qps: u32,
    }

    impl Default for Limits {
        fn default() -> Self {
            Self { qps: 100 }
        }
    }

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    struct All {
        feature: Feature,
        limits: Limits,
    }

    /// 改动态配置的表用的管理员连接
    enum Admin {
        Pg(sqlx::PgPool),
        MySql(sqlx::MySqlPool),
        Sqlite(sqlx::SqlitePool),
    }

    /// 一个源的配置表（带 schema / 库名），模拟别的程序直接改表
    struct Table {
        admin: Admin,
        name: &'static str,
    }

    impl Table {
        /// MySQL 的 key 是保留字，要反引号
        fn key(&self) -> &'static str {
            match self.admin {
                Admin::MySql(_) => "`key`",
                _ => "key",
            }
        }

        async fn exec(&self, sql: String) {
            let sql = sqlx::AssertSqlSafe(sql);
            match &self.admin {
                Admin::Pg(pool) => sqlx::raw_sql(sql).execute(pool).await.map(|_| ()),
                Admin::MySql(pool) => sqlx::raw_sql(sql).execute(pool).await.map(|_| ()),
                Admin::Sqlite(pool) => sqlx::raw_sql(sql).execute(pool).await.map(|_| ()),
            }
            .unwrap();
        }

        async fn set(&self, namespace: &str, key: &str, json: &str) {
            let (table, k) = (self.name, self.key());
            self.exec(format!(
                "UPDATE {table} SET value = '{json}' WHERE namespace = '{namespace}' AND {k} = '{key}'"
            ))
            .await;
        }

        async fn insert(&self, namespace: &str, key: &str, json: &str) {
            let (table, k) = (self.name, self.key());
            self.exec(format!(
                "INSERT INTO {table} (namespace, {k}, value) VALUES ('{namespace}', '{key}', '{json}')"
            ))
            .await;
        }

        async fn delete(&self, namespace: &str, key: &str) {
            let (table, k) = (self.name, self.key());
            self.exec(format!(
                "DELETE FROM {table} WHERE namespace = '{namespace}' AND {k} = '{key}'"
            ))
            .await;
        }

        /// 历史表里这个 key 的 op（I / U / D），按发生顺序
        async fn history(&self, namespace: &str, key: &str) -> Vec<String> {
            let (table, k) = (self.name, self.key());
            let sql = sqlx::AssertSqlSafe(format!(
                "SELECT op FROM {table}_history WHERE namespace = '{namespace}' AND {k} = '{key}' ORDER BY id"
            ));
            match &self.admin {
                Admin::Pg(pool) => sqlx::query_scalar(sql).fetch_all(pool).await,
                Admin::MySql(pool) => sqlx::query_scalar(sql).fetch_all(pool).await,
                Admin::Sqlite(pool) => sqlx::query_scalar(sql).fetch_all(pool).await,
            }
            .unwrap()
        }
    }

    /// 一个源上走一遍：bind 和 bind_all 同时绑着；改值（数字、数组、布尔）、另一个 namespace、表里多出来的 key、
    /// 删 key 退回默认值、转不了的值保留旧值；最后看历史。`base` 让各个源的值不一样，确认各收各的
    async fn exercise(name: &str, source: DbLiveConfig, table: Table, base: u32) {
        let mut one = source.bind("feature", Feature::default()).await.unwrap();
        let all = source.bind_all(All::default()).await.unwrap();
        assert_eq!(*one.get(), Feature::default(), "{name}");
        assert_eq!(*all.get(), All::default(), "{name}");

        // 改值：changed() 收到，两个绑定都看到
        table.set("feature", "level", &base.to_string()).await;
        tokio::time::timeout(Duration::from_secs(5), one.changed())
            .await
            .unwrap_or_else(|_| panic!("{name}: changed() 没收到"))
            .unwrap();
        assert_eq!(one.get().level, base, "{name}");
        eventually(5, || async { all.get().feature.level == base }).await;

        // 数组、布尔
        table.set("feature", "tags", r#"["x", "y"]"#).await;
        table.set("feature", "enabled", "true").await;
        eventually(5, || async {
            let f = one.get();
            f.enabled && f.tags == ["x", "y"]
        })
        .await;

        // 另一个 namespace：只有 bind_all 那份变
        table.set("limits", "qps", &(base + 1).to_string()).await;
        eventually(5, || async { all.get().limits.qps == base + 1 }).await;
        assert_eq!(one.get().level, base, "{name}");

        // 表里多出 struct 没有的 key：忽略，其余照常更新
        table.insert("feature", "unknown", "1").await;
        table.set("feature", "level", &(base + 2).to_string()).await;
        eventually(5, || async { one.get().level == base + 2 }).await;

        // 删 key：退回默认值
        table.delete("feature", "level").await;
        eventually(5, || async { one.get().level == 1 }).await;

        // 转不了的值：保留旧值；改回合法的值后照常更新
        table.set("feature", "enabled", r#""abc""#).await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(one.get().enabled, "{name}: 转不了，保留旧值");
        table.set("feature", "enabled", "false").await;
        eventually(5, || async { !one.get().enabled }).await;

        assert_eq!(
            table.history("feature", "level").await,
            ["I", "U", "U", "D"],
            "{name}"
        );
    }

    /// 4 个源（PG 两个靠 NOTIFY，MySQL、SQLite 靠轮询）都绑同一个 namespace，各改各的表，各收各的
    #[tokio::test]
    async fn live_config_every_source() {
        let env = Env::new().await;
        let res = env.res();
        let source = |name: &str| res.get_named::<DbLiveConfig>(name).unwrap();
        let sources = [
            (
                "main",
                Table {
                    admin: Admin::Pg(env.pg_admin("app").await),
                    name: "cfg.hy_config",
                },
                10,
            ),
            (
                "platform",
                Table {
                    admin: Admin::Pg(env.pg_admin("platform").await),
                    name: "platform_config",
                },
                20,
            ),
            (
                "shop",
                Table {
                    admin: Admin::MySql(env.mysql_root().await),
                    name: "shop.hy_config",
                },
                30,
            ),
            (
                "local",
                Table {
                    admin: Admin::Sqlite(env.local_sqlite().await),
                    name: "hy_config",
                },
                40,
            ),
        ];
        let mut runs = tokio::task::JoinSet::new();
        for (name, table, base) in sources {
            let live = source(name);
            runs.spawn(async move { exercise(name, live, table, base).await });
        }
        while let Some(result) = runs.join_next().await {
            result.unwrap();
        }

        // 同一个 namespace 在 4 个源里各是各的值
        let mut qps = Vec::new();
        for name in ["main", "platform", "shop", "local"] {
            let all = source(name).bind_all(All::default()).await.unwrap();
            qps.push(all.get().limits.qps);
        }
        assert_eq!(qps, [11, 21, 31, 41]);
    }

    // ---- 4. 锁：两个实例 -------------------------------------------------------------------

    /// 再起一个 Registry 当第二个实例，同一份配置。锁在 MySQL 上（legacy 组件、locker 用户单独建的池）
    #[tokio::test]
    async fn lock_between_two_instances() {
        let env = Env::new().await;
        let second = env.instance().await;
        let a = env.res().get::<DbLockClient>().unwrap();
        let b = second.res.get::<DbLockClient>().unwrap();
        let root = env.mysql_root().await;
        let key = || lock_key!(TestLockKey::Job, 1);

        // 字符串 key：别的实例拿不到，自己再拿也拿不到（不可重入），别的 key 不受影响；持锁的连接是 locker 的
        let guard = a.try_lock(key()).await.unwrap().unwrap();
        assert_eq!(guard.key(), "999100002:job:1");
        assert!(b.try_lock(key()).await.unwrap().is_none());
        assert!(a.try_lock(key()).await.unwrap().is_none());
        b.try_lock(lock_key!(TestLockKey::Job, 2))
            .await
            .unwrap()
            .unwrap()
            .release()
            .await
            .unwrap();
        let holder: String = sqlx::query_scalar(
            "SELECT USER FROM information_schema.PROCESSLIST WHERE ID = IS_USED_LOCK(?)",
        )
        .bind(guard.key())
        .fetch_one(&root)
        .await
        .unwrap();
        assert_eq!(holder, "locker");
        guard.release().await.unwrap();
        b.try_lock(key())
            .await
            .unwrap()
            .unwrap()
            .release()
            .await
            .unwrap();

        // i64
        let guard = a.try_lock_i64(7).await.unwrap().unwrap();
        assert_eq!(guard.key(), "i64:7");
        assert!(b.try_lock_i64(7).await.unwrap().is_none());
        assert!(b.try_lock_i64(8).await.unwrap().is_some());
        guard.release().await.unwrap();
        b.try_lock_i64(7)
            .await
            .unwrap()
            .unwrap()
            .release()
            .await
            .unwrap();

        // 锁类型 + i32：和 i64 是两个 key 空间
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
            b.try_lock_i32(TestLockType::Job, 43)
                .await
                .unwrap()
                .is_some()
        );
        assert!(b.try_lock_i64(42).await.unwrap().is_some(), "和 i64 不撞");
        pair.release().await.unwrap();

        // 阻塞版：a 持有三种 key，b 等满 1 秒返回 None；timeout 为 0 等于 try
        let held = [
            a.try_lock(key()).await.unwrap().unwrap(),
            a.try_lock_i64(7).await.unwrap().unwrap(),
            a.try_lock_i32(TestLockType::Job, 42)
                .await
                .unwrap()
                .unwrap(),
        ];
        let start = tokio::time::Instant::now();
        assert!(
            b.lock(key(), Duration::from_secs(1))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            b.lock_i64(7, Duration::from_secs(1))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            b.lock_i32(TestLockType::Job, 42, Duration::from_secs(1))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            start.elapsed() >= Duration::from_millis(2700),
            "三次各等满约 1 秒：{:?}",
            start.elapsed()
        );
        assert!(b.lock(key(), Duration::ZERO).await.unwrap().is_none());

        // 阻塞版：a 一释放，b 马上拿到
        let waiters = [
            tokio::spawn({
                let b = b.clone();
                async move { b.lock(key(), Duration::from_secs(10)).await }
            }),
            tokio::spawn({
                let b = b.clone();
                async move { b.lock_i64(7, Duration::from_secs(10)).await }
            }),
            tokio::spawn({
                let b = b.clone();
                async move {
                    b.lock_i32(TestLockType::Job, 42, Duration::from_secs(10))
                        .await
                }
            }),
        ];
        tokio::time::sleep(Duration::from_millis(500)).await;
        let released = tokio::time::Instant::now();
        for guard in held {
            guard.release().await.unwrap();
        }
        let mut got = Vec::new();
        for waiter in waiters {
            got.push(waiter.await.unwrap().unwrap().unwrap());
        }
        assert!(
            released.elapsed() < Duration::from_secs(1),
            "释放了马上拿到：{:?}",
            released.elapsed()
        );
        let keys: Vec<&str> = got.iter().map(|g| g.key()).collect();
        assert_eq!(keys, ["999100002:job:1", "i64:7", "i32:999100001:42"]);
        for guard in got {
            guard.release().await.unwrap();
        }

        // guard 没 release 就 drop：后台释放，别人随后能拿到
        drop(a.try_lock(key()).await.unwrap().unwrap());
        eventually(5, || async {
            match b.try_lock(key()).await.unwrap() {
                Some(guard) => {
                    guard.release().await.unwrap();
                    true
                }
                None => false,
            }
        })
        .await;

        // 参数不对：不发 SQL 就报错。MySQL 的 timeout 要整数秒，锁名最长 64 个字符
        let e = a
            .lock_i64(1, Duration::from_millis(1500))
            .await
            .unwrap_err();
        assert!(e.is(LockErr::InvalidTimeout), "{e:#}");
        let e = a
            .try_lock(lock_key!(TestLockKey::Job, "x".repeat(60)))
            .await
            .unwrap_err();
        assert!(e.is(LockErr::InvalidKey), "{e:#}");
    }
}
