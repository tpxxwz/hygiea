//! `SeaOrmPgPool`（`src/seaorm/pool/postgres.rs`，feature `seaorm` + `postgres`）的集成测试，feature 全开（`lock-row`、`live-config`），container 层：
//! 用代码启动官方 postgres 容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）
//! ```sh
//! cargo test -p hygiea-db --features seaorm,postgres,lock-row,live-config --test seaorm_postgres -- --ignored ::container::
//! ```
//! 锁、动态配置和 `SqlxPgPool` 是同一份实现，完整的测试在 `sqlx_postgres.rs`；这里确认 SeaORM 的连接池接上了

#![cfg(all(
    feature = "seaorm",
    feature = "postgres",
    feature = "lock-row",
    feature = "live-config"
))]

mod common;

use common::eventually;
use hygiea_core::lock::{hy_lock_key, lock_key};
use hygiea_test::container;
use serde::{Deserialize, Serialize};

#[derive(hy_lock_key)]
#[lock_internal_module_prefix = "100"]
enum TestLockKey {
    #[lock_key(code = "002", tpl = "job:{{ id }}")]
    Job,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Cache {
    ttl_secs: u64,
}

/// 连接池本身：连接、search_path、经 Registry 启动和停止、HyUtcDateTime、事务、`db!`、分页
#[container]
mod pool {
    use hygiea_core::app::{ConfigResource, Registry, Resources};
    use hygiea_core::datetime::{HygieaDateTimeExt, UtcDateTime};
    use hygiea_db::HyUtcDateTime;
    use hygiea_db::{PgConnectConfig, SeaOrmPgComponent, SeaOrmPgConfig, SeaOrmPgPool};
    use hygiea_test::container::{ContainerSpec, RunningContainer};
    use sea_orm::{ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, Set, Statement};

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

    fn seaorm_config(server: &RunningContainer) -> SeaOrmPgConfig {
        SeaOrmPgConfig {
            connect: connect(server),
            ..Default::default()
        }
    }

    /// 用默认配置连上去建好 [`SCHEMA`]，search_path 指向不存在的 schema 时 current_schema() 是 NULL
    async fn create_schema(server: &RunningContainer) {
        let pool = SeaOrmPgPool::from_config(&seaorm_config(server), &Default::default())
            .await
            .unwrap();
        pool.execute_unprepared("CREATE SCHEMA hygiea_test")
            .await
            .unwrap();
        pool.close().await.unwrap();
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

    fn sql(s: &str) -> Statement {
        Statement::from_string(DbBackend::Postgres, s)
    }

    #[tokio::test]
    async fn seaorm_connect_success() {
        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&seaorm_config(&server), &Default::default())
            .await
            .unwrap();

        let row = pool.query_one_raw(sql("SELECT 1")).await.unwrap().unwrap();
        assert_eq!(row.try_get_by_index::<i32>(0).unwrap(), 1);
    }

    #[tokio::test]
    async fn seaorm_schema_search_path_takes_effect() {
        let server = start().await;
        create_schema(&server).await;

        let mut config = seaorm_config(&server);
        config.connect.schema_search_path = SCHEMA.to_string();
        let pool = SeaOrmPgPool::from_config(&config, &Default::default())
            .await
            .unwrap();

        let row = pool
            .query_one_raw(sql("SELECT current_schema()"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get_by_index::<Option<String>>(0)
                .unwrap()
                .as_deref(),
            Some(SCHEMA)
        );
    }

    #[tokio::test]
    async fn seaorm_stop_closes_connection() {
        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&seaorm_config(&server), &Default::default())
            .await
            .unwrap();

        pool.query_one_raw(sql("SELECT 1")).await.unwrap();

        // close 关的是共享的底层连接池，pool 本身还能用来发查询
        pool.close().await.unwrap();

        // 关闭后再发查询应该失败
        let result = pool.query_one_raw(sql("SELECT 1")).await;
        assert!(result.is_err(), "{result:?}");
    }

    /// SeaOrmPgComponent 经 Registry 启动，按组件名取到能用的连接
    #[tokio::test]
    async fn seaorm_component_startup_via_registry() {
        let server = start().await;
        let registry =
            Registry::new().add_named::<SeaOrmPgComponent>("primary", seaorm_config(&server));
        let pool = with_resources(registry, |res| res.get_named::<SeaOrmPgPool>("primary"))
            .await
            .unwrap();

        let row = pool.query_one_raw(sql("SELECT 1")).await.unwrap().unwrap();
        assert_eq!(row.try_get_by_index::<i32>(0).unwrap(), 1);
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

    /// SeaORM 实体，字段直接用 `HyUtcDateTime`
    mod record {
        use hygiea_db::HyUtcDateTime;
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "record")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: i64,
            pub create_at: HyUtcDateTime,
            pub delete_at: Option<HyUtcDateTime>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    /// SeaORM 实体字段用 `HyUtcDateTime`，插入再按主键查回，可空字段 None 对应 NULL；
    /// 库里按 +08:00 写的值读出来换算成 UTC
    #[tokio::test]
    async fn seaorm_hy_utc_datetime_round_trip() {
        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&seaorm_config(&server), &Default::default())
            .await
            .unwrap();
        pool.execute_unprepared(CREATE_RECORD).await.unwrap();

        let at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        for (id, delete_at) in [(1, Some(at)), (2, None)] {
            record::ActiveModel {
                id: Set(id),
                create_at: Set(at),
                delete_at: Set(delete_at),
            }
            .insert(&*pool)
            .await
            .unwrap();
        }

        let rows = record::Entity::find().all(&*pool).await.unwrap();
        let mut rows: Vec<_> = rows
            .into_iter()
            .map(|m| (m.id, m.create_at, m.delete_at))
            .collect();
        rows.sort_by_key(|r| r.0);
        assert_eq!(rows, vec![(1, at, Some(at)), (2, at, None)]);

        pool.execute_unprepared(
            "INSERT INTO record (id, create_at) VALUES (3, '2026-10-05 20:57:24.719+08')",
        )
        .await
        .unwrap();
        let shanghai = record::Entity::find_by_id(3_i64)
            .one(&*pool)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(shanghai.create_at, at);
    }

    /// seaorm::transaction：闭包 Ok 就提交；Err 就回滚，闭包的错误原样返回
    #[tokio::test]
    async fn seaorm_transaction_commits_or_rolls_back() {
        use hygiea_core::{ResultExt, err};
        use hygiea_db::RepoErr;

        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&seaorm_config(&server), &Default::default())
            .await
            .unwrap();
        pool.execute_unprepared("CREATE TABLE item (id INT PRIMARY KEY)")
            .await
            .unwrap();
        let insert = async |txn: &sea_orm::DatabaseTransaction, id: i32| {
            txn.execute_unprepared(&format!("INSERT INTO item VALUES ({id})"))
                .await
                .wrap_err(|| err!(RepoErr::InsertFailed, "insert item"))
        };

        hygiea_db::seaorm::transaction(&*pool, "save item", async |txn| {
            insert(txn, 1).await?;
            insert(txn, 2).await?;
            Ok(())
        })
        .await
        .unwrap();

        let e = hygiea_db::seaorm::transaction(&*pool, "save item", async |txn| {
            insert(txn, 3).await?;
            insert(txn, 1).await?;
            Ok(())
        })
        .await
        .unwrap_err();
        assert!(e.is(RepoErr::InsertFailed), "{e:#}");

        let rows = pool
            .query_all_raw(sql("SELECT id FROM item ORDER BY id"))
            .await
            .unwrap();
        let ids: Vec<i32> = rows.iter().map(|r| r.try_get("", "id").unwrap()).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    /// 复合主键 (a, b) 的实体，不建表，测试里用来看报错
    mod pair {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "pair")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub a: i64,
            #[sea_orm(primary_key, auto_increment = false)]
            pub b: i64,
            pub v: i64,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    /// 普通查询用 SeaORM + db!：into_tuple 这类转换也整条放进 db!；插入用 active_models 把 &[Model] 转过去
    #[tokio::test]
    async fn seaorm_db_select() {
        use hygiea_db::db;
        use hygiea_db::seaorm::active_models;
        use sea_orm::{QueryOrder, QuerySelect};

        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&seaorm_config(&server), &Default::default())
            .await
            .unwrap();
        pool.execute_unprepared(CREATE_RECORD).await.unwrap();
        let at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let models = [1, 2, 3].map(|id| record::Model {
            id,
            create_at: at,
            delete_at: None,
        });
        db!(record::Entity::insert_many(active_models(&models)).exec_without_returning(&*pool))
            .unwrap();

        // 普通查询：SeaORM 的调用整条放进 db!；into_tuple 之后拿不到表名，要写 op
        let ids: Vec<i64> = db!(
            record::Entity::find()
                .select_only()
                .column(record::Column::Id)
                .order_by_desc(record::Column::Id)
                .into_tuple()
                .all(&*pool),
            "record ids"
        )
        .unwrap();
        assert_eq!(ids, [3, 2, 1]);
        let first = db!(record::Entity::find()
            .order_by_asc(record::Column::Id)
            .one(&*pool))
        .unwrap();
        assert_eq!(first, Some(models[0].clone()));
    }

    /// 单条插入直接用 SeaORM，db! 包住：失败自动是 InsertFailed、op 里有表名
    #[tokio::test]
    async fn seaorm_db_insert() {
        use hygiea_db::RepoErr;
        use hygiea_db::db;
        use sea_orm::{IntoActiveModel, TryInsertResult, sea_query::OnConflict};

        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&seaorm_config(&server), &Default::default())
            .await
            .unwrap();
        pool.execute_unprepared(CREATE_RECORD).await.unwrap();
        let at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let later = HyUtcDateTime(utc("2026-10-06T00:00:00Z"));
        let model = |id, delete_at| record::Model {
            id,
            create_at: at,
            delete_at,
        };
        let db = &*pool;

        // insert_one：返回行数
        let rows =
            db!(record::Entity::insert(model(1, None).into_active_model())
                .exec_without_returning(db))
            .unwrap();
        assert_eq!(rows, 1);

        // insert_one_returning：返回写入后的整行
        let written =
            db!(record::Entity::insert(model(2, None).into_active_model()).exec_with_returning(db))
                .unwrap();
        assert_eq!(written, model(2, None));

        // 主键冲突：InsertFailed，op 里有表名，原始错误在 source 上
        let e =
            db!(record::Entity::insert(model(1, None).into_active_model())
                .exec_without_returning(db))
            .unwrap_err();
        assert!(e.is(RepoErr::InsertFailed), "{e:#}");
        assert!(
            format!("{e:#}").contains("Db insert failed: record"),
            "{e:#}"
        );
        assert!(format!("{e:#}").contains("duplicate key"), "{e:#}");

        // insert_one_ignore_conflict：TryInsert，冲突时是 Conflicted 或 0 行
        let skipped = db!(
            record::Entity::insert(model(1, Some(later)).into_active_model())
                .on_conflict_do_nothing()
                .exec_without_returning(db)
        )
        .unwrap();
        assert!(
            matches!(skipped, TryInsertResult::Inserted(0)),
            "{skipped:?}"
        );

        // insert_one_or_update_returning：on_conflict 由调用方用 SeaORM 的 OnConflict 拼
        let upsert = OnConflict::column(record::Column::Id)
            .update_column(record::Column::DeleteAt)
            .to_owned();
        let written = db!(
            record::Entity::insert(model(1, Some(later)).into_active_model())
                .on_conflict(upsert)
                .exec_with_returning(db)
        )
        .unwrap();
        assert_eq!(written, model(1, Some(later)));
    }

    /// db! 的 op 是"表名 at 文件:行号"，跨行时行号是 db! 开头那一行；第二个参数是自定义 op，写在表名前面；
    /// ActiveModel 的方法按方法名定操作；SeaORM 类型里没有实体的（into_tuple 之后、分页器）必须写 op。.db() 一律 ExecFailed，op 是调用位置
    #[tokio::test]
    async fn seaorm_db_errors() {
        use hygiea_db::RepoErr;
        use hygiea_db::db;
        use hygiea_db::seaorm::DbResultExt;
        use sea_orm::{ActiveModelTrait, IntoActiveModel, ModelTrait, PaginatorTrait, QuerySelect};

        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&seaorm_config(&server), &Default::default())
            .await
            .unwrap();
        let db = &*pool;
        let op = |e: &hygiea_core::HyErr| format!("{e}");

        // 表没建：查、增、改、删各自的错误，op 带表名和 db! 那一行
        let line = line!() + 1;
        let e = db!(record::Entity::find().all(db)).unwrap_err();
        assert!(e.is(RepoErr::QueryFailed), "{e:#}");
        assert_eq!(
            op(&e),
            format!("Db query failed: record at {}:{line}", file!())
        );
        // 跨行时是 db! 开头那一行
        let line = line!() + 1;
        let e = db!(record::Entity::find()
            .select_only()
            .column(record::Column::Id)
            .count(db))
        .unwrap_err();
        assert_eq!(
            op(&e),
            format!("Db query failed: record at {}:{line}", file!())
        );

        let am = record::Model {
            id: 1,
            create_at: HyUtcDateTime(utc("2026-10-05T12:57:24.719Z")),
            delete_at: None,
        }
        .into_active_model();
        let e = db!(am.clone().insert(db)).unwrap_err();
        assert!(e.is(RepoErr::InsertFailed), "{e:#}");
        assert!(op(&e).starts_with("Db insert failed: record at "), "{e:#}");
        let e = db!(am.clone().update(db)).unwrap_err();
        assert!(e.is(RepoErr::UpdateFailed), "{e:#}");
        let e = db!(am.clone().delete(db)).unwrap_err();
        assert!(e.is(RepoErr::DeleteFailed), "{e:#}");
        // Model 也有 delete（ModelTrait::delete），和 ActiveModel 的同名
        let model = record::Model {
            id: 1,
            create_at: HyUtcDateTime(utc("2026-10-05T12:57:24.719Z")),
            delete_at: None,
        };
        let e = db!(model.delete(db)).unwrap_err();
        assert!(e.is(RepoErr::DeleteFailed), "{e:#}");
        assert!(op(&e).starts_with("Db delete failed: record at "), "{e:#}");
        // ActiveModel 也能写 op，表名照样带上
        let line = line!() + 1;
        let e = db!(am.clone().delete(db), "remove record").unwrap_err();
        assert_eq!(
            op(&e),
            format!(
                "Db delete failed: remove record, record at {}:{line}",
                file!()
            )
        );
        // save 分不出增还是改
        let e = db!(am.clone().save(db)).unwrap_err();
        assert!(e.is(RepoErr::ExecFailed), "{e:#}");
        let e = db!(record::Entity::delete_many().exec(db)).unwrap_err();
        assert!(e.is(RepoErr::DeleteFailed), "{e:#}");
        assert!(op(&e).starts_with("Db delete failed: record at "), "{e:#}");
        // 拿得到表名的也能写 op：自定义在前，表名照样带上
        let line = line!() + 1;
        let e = db!(record::Entity::find().all(db), "list records").unwrap_err();
        assert_eq!(
            op(&e),
            format!(
                "Db query failed: list records, record at {}:{line}",
                file!()
            )
        );

        // into_tuple 之后、分页器拿不到表名，必须写 op（不写编译报错），op 是"自定义 at 位置"
        let line = line!() + 1;
        let e = db!(
            record::Entity::find()
                .select_only()
                .column(record::Column::Id)
                .into_tuple::<i64>()
                .all(db),
            "record ids"
        )
        .unwrap_err();
        assert!(e.is(RepoErr::QueryFailed), "{e:#}");
        assert_eq!(
            op(&e),
            format!("Db query failed: record ids at {}:{line}", file!())
        );
        let e = db!(
            record::Entity::find().paginate(db, 10).fetch_page(0),
            "record page"
        )
        .unwrap_err();
        assert!(e.is(RepoErr::QueryFailed), "{e:#}");
        assert!(
            op(&e).starts_with("Db query failed: record page at "),
            "{e:#}"
        );

        // 原始错误在 source 上
        assert!(
            format!("{e:#}").contains(r#"relation "record" does not exist"#),
            "{e:#}"
        );

        // 连接上直接执行：ExecFailed，拿不到表名，要写 op
        let line = line!() + 1;
        let e = db!(db.execute_unprepared("SELECT * FROM record"), "raw select").unwrap_err();
        assert!(e.is(RepoErr::ExecFailed), "{e:#}");
        assert_eq!(
            op(&e),
            format!("Db exec failed: raw select at {}:{line}", file!())
        );

        // .db()：什么写法都能接，ExecFailed，op 是 .db() 所在的行（跨行时不是语句开头那行）
        let result = db.execute_unprepared("SELECT * FROM record").await;
        let line = line!() + 1;
        let e = result.db().unwrap_err();
        assert!(e.is(RepoErr::ExecFailed), "{e:#}");
        assert_eq!(op(&e), format!("Db exec failed: {}:{line}", file!()));
        db.execute_unprepared(CREATE_RECORD).await.db().unwrap();
        assert_eq!(db!(record::Entity::find().count(db)).unwrap(), 0);
    }

    /// find_page 取一页加总数：中间页、最后一页不满、页号超出时 items 为空，总数和总页数不变
    #[tokio::test]
    async fn seaorm_find_page() {
        use hygiea_db::db;
        use hygiea_db::seaorm::find_page;
        use hygiea_db::{Page, PageRequest, RepoErr};
        use sea_orm::{ColumnTrait, IntoActiveModel, QueryFilter, QueryOrder};

        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&seaorm_config(&server), &Default::default())
            .await
            .unwrap();
        pool.execute_unprepared(CREATE_RECORD).await.unwrap();
        let at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let models = (1..=7).map(|id| {
            record::Model {
                id,
                create_at: at,
                delete_at: None,
            }
            .into_active_model()
        });
        db!(record::Entity::insert_many(models).exec_without_returning(&*pool)).unwrap();

        let size = 3;
        let ids = async |page| {
            let select = record::Entity::find()
                .filter(record::Column::Id.gte(2))
                .order_by_asc(record::Column::Id);
            find_page(&*pool, select, PageRequest::new(page, size).unwrap())
                .await
                .unwrap()
                .map(|m| m.id)
        };
        assert_eq!(
            ids(0).await,
            Page::new(vec![2, 3, 4], PageRequest::new(0, size).unwrap(), 6)
        );
        assert_eq!(
            ids(1).await,
            Page::new(vec![5, 6, 7], PageRequest::new(1, size).unwrap(), 6)
        );
        let beyond = ids(2).await;
        assert_eq!(beyond.items, Vec::<i64>::new());
        assert_eq!((beyond.total_items, beyond.total_pages), (6, 2));

        // pair 表没建
        let e = find_page(
            &*pool,
            pair::Entity::find(),
            PageRequest::new(4, size).unwrap(),
        )
        .await
        .unwrap_err();
        assert!(e.is(RepoErr::QueryFailed), "{e:#}");
        assert!(
            format!("{e:#}").contains("Db query failed: find_page pair page 4 size 3, fetch items"),
            "{e:#}"
        );
    }

    /// transaction_snapshot 开出来是 REPEATABLE READ + READ ONLY，里面写数据报错；transaction 是默认的 READ COMMITTED
    #[tokio::test]
    async fn seaorm_transaction_snapshot_is_repeatable_read_only() {
        use hygiea_core::{ResultExt, err};
        use hygiea_db::RepoErr;

        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&seaorm_config(&server), &Default::default())
            .await
            .unwrap();
        pool.execute_unprepared(CREATE_RECORD).await.unwrap();
        async fn show(txn: &impl ConnectionTrait, name: &str) -> String {
            txn.query_one_raw(sql(&format!("SHOW {name}")))
                .await
                .unwrap()
                .unwrap()
                .try_get("", name)
                .unwrap()
        }

        let (isolation, read_only) =
            hygiea_db::seaorm::transaction_snapshot(&*pool, "read", async |txn| {
                Ok((
                    show(txn, "transaction_isolation").await,
                    show(txn, "transaction_read_only").await,
                ))
            })
            .await
            .unwrap();
        assert_eq!(
            (isolation.as_str(), read_only.as_str()),
            ("repeatable read", "on")
        );

        let written = hygiea_db::seaorm::transaction_snapshot(&*pool, "write", async |txn| {
            txn.execute_unprepared("INSERT INTO record (id, create_at) VALUES (1, now())")
                .await
                .wrap_err(|| err!(RepoErr::InsertFailed, "insert record"))?;
            Ok(())
        })
        .await;
        assert!(written.unwrap_err().is(RepoErr::InsertFailed));

        let isolation = hygiea_db::seaorm::transaction(&*pool, "read", async |txn| {
            Ok(show(txn, "transaction_isolation").await)
        })
        .await
        .unwrap();
        assert_eq!(isolation, "read committed");
    }
}

/// 分布式锁：两个 SeaORM 连接池模拟两个实例
#[container]
mod lock {
    use hygiea_core::app::{ConfigResource, RegistryConfig};
    use hygiea_db::{PgConnectConfig, SeaOrmPgConfig, SeaOrmPgPool};
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

    async fn pool(server: &RunningContainer) -> SeaOrmPgPool {
        let config = SeaOrmPgConfig {
            connect: connect(server),
            ..Default::default()
        };
        SeaOrmPgPool::from_config(&config, &app("hy"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn i64_is_exclusive() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        common::i64_is_exclusive(&a, &b).await;
    }

    #[tokio::test]
    async fn blocking_waits_and_times_out() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        common::blocking_waits_and_times_out(&a, &b).await;
    }

    /// 字符串 key 走锁表
    #[tokio::test]
    async fn string_key_is_exclusive() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        common::string_key_is_exclusive(&a, &b, || lock_key!(TestLockKey::Job, 1)).await;
    }
}

/// 动态配置：SeaORM 连接池交出连接参数，另开小池
#[container]
mod live_config {
    use hygiea_core::app::{ConfigResource, RegistryConfig};
    use hygiea_db::{LiveConfigConfig, PgConnectConfig, SeaOrmPgConfig, SeaOrmPgPool};
    use hygiea_test::container::{ContainerSpec, RunningContainer};
    use sea_orm::ConnectionTrait;

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

    fn app() -> RegistryConfig {
        RegistryConfig {
            app_name: "hy".to_string(),
            ..Default::default()
        }
    }

    /// 轮询间隔很长：测试里的变化只能靠通知收到
    fn config(server: &RunningContainer, search_path: &str) -> SeaOrmPgConfig {
        SeaOrmPgConfig {
            connect: PgConnectConfig {
                host: server.host().to_string(),
                port: server.port(PORT),
                password: PASSWORD.to_string(),
                schema_search_path: search_path.to_string(),
                ..Default::default()
            },
            live_config: LiveConfigConfig {
                enabled: true,
                poll_interval_secs: Some(3600),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// 第一次 bind 建表、补默认值；改表后 NOTIFY 推过来
    #[tokio::test]
    async fn update_is_pushed_by_notify() {
        let server = start().await;
        let pool = SeaOrmPgPool::from_config(&config(&server, ""), &app())
            .await
            .unwrap();
        let cache = pool.live_config("cache", Cache::default()).await.unwrap();
        assert_eq!(cache.get().ttl_secs, 0);
        pool.execute_unprepared("UPDATE hy_config SET value = '8' WHERE namespace = 'cache'")
            .await
            .unwrap();
        eventually(5, || async { cache.get().ttl_secs == 8 }).await;
        pool.close().await.unwrap();
    }

    /// 动态配置跟着连接池的 search_path，表建在那个 schema 下
    #[tokio::test]
    async fn follows_search_path() {
        let server = start().await;
        let admin = SeaOrmPgPool::from_config(&config(&server, ""), &app())
            .await
            .unwrap();
        admin.execute_unprepared("CREATE SCHEMA app").await.unwrap();
        let pool = SeaOrmPgPool::from_config(&config(&server, "app"), &app())
            .await
            .unwrap();
        let cache = pool.live_config("cache", Cache::default()).await.unwrap();
        let created = admin
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT to_regclass('app.hy_config') IS NOT NULL",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index::<bool>(0)
            .unwrap();
        assert!(created);
        admin
            .execute_unprepared("UPDATE app.hy_config SET value = '8' WHERE namespace = 'cache'")
            .await
            .unwrap();
        eventually(5, || async { cache.get().ttl_secs == 8 }).await;
        pool.close().await.unwrap();
    }
}
