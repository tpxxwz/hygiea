//! PostgreSQL 连接池集成测试（sqlx 和 seaorm），container 层（见仓库根目录 AGENTS.md 的测试分层）：
//! 用代码启动官方 postgres 容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）
//! ```sh
//! cargo test -p hygiea-db --all-features --test pg -- --ignored ::container::
//! ```

#![cfg(all(feature = "sqlx", feature = "seaorm", feature = "postgres"))]

use hygiea_test::container;

/// 用代码启动的官方 postgres 镜像
#[container]
mod postgres {
    use hygiea_core::app::{Registry, Resources};
    use hygiea_core::datetime::{HygieaDateTimeExt, UtcDateTime};
    use hygiea_db::HyUtcDateTime;
    use hygiea_db::{
        SeaOrmPgComponent, SeaOrmPgConfig, SeaOrmPgPool, SqlxPgComponent, SqlxPgConfig, SqlxPgPool,
    };
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
        ContainerSpec::new("postgres", "17")
            .env("POSTGRES_USER", USER)
            .env("POSTGRES_PASSWORD", PASSWORD)
            .env("POSTGRES_DB", DATABASE)
            .port(PORT)
            .wait_log_times("database system is ready to accept connections", 2)
            .start()
            .await
    }

    fn sqlx_config(server: &RunningContainer) -> SqlxPgConfig {
        SqlxPgConfig {
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
            host: server.host().to_string(),
            port: server.port(PORT),
            username: USER.to_string(),
            password: PASSWORD.to_string(),
            database: DATABASE.to_string(),
            ..Default::default()
        }
    }

    /// 用默认配置连上去建好 [`SCHEMA`]，search_path 指向不存在的 schema 时 current_schema() 是 NULL
    async fn create_schema(server: &RunningContainer) {
        let pool = SqlxPgPool::connect(sqlx_config(server)).await.unwrap();
        sqlx::query("CREATE SCHEMA hygiea_test")
            .execute(&*pool)
            .await
            .unwrap();
        pool.inner.close().await;
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
    async fn sqlx_connect_success() {
        let server = start().await;
        let pool = SqlxPgPool::connect(sqlx_config(&server)).await.unwrap();

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

        let config = SqlxPgConfig {
            schema_search_path: SCHEMA.to_string(),
            ..sqlx_config(&server)
        };
        let pool = SqlxPgPool::connect(config).await.unwrap();

        let schema: Option<String> = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(&*pool)
            .await
            .unwrap();
        assert_eq!(schema.as_deref(), Some(SCHEMA));
    }

    #[tokio::test]
    async fn sqlx_stop_closes_pool() {
        let server = start().await;
        let config = SqlxPgConfig {
            max_connections: Some(2),
            ..sqlx_config(&server)
        };
        let pool = SqlxPgPool::connect(config).await.unwrap();

        sqlx::query("SELECT 1").fetch_one(&*pool).await.unwrap();

        pool.inner.close().await;

        // 关闭后再发查询应该失败
        let err = sqlx::query("SELECT 1").fetch_one(&*pool).await.unwrap_err();
        assert!(matches!(err, sqlx::Error::PoolClosed), "{err:?}");
    }

    #[tokio::test]
    async fn seaorm_connect_success() {
        let server = start().await;
        let pool = SeaOrmPgPool::connect(seaorm_config(&server)).await.unwrap();

        let row = pool.query_one_raw(sql("SELECT 1")).await.unwrap().unwrap();
        assert_eq!(row.try_get_by_index::<i32>(0).unwrap(), 1);
    }

    #[tokio::test]
    async fn seaorm_schema_search_path_takes_effect() {
        let server = start().await;
        create_schema(&server).await;

        let config = SeaOrmPgConfig {
            schema_search_path: SCHEMA.to_string(),
            ..seaorm_config(&server)
        };
        let pool = SeaOrmPgPool::connect(config).await.unwrap();

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
        let pool = SeaOrmPgPool::connect(seaorm_config(&server)).await.unwrap();

        pool.query_one_raw(sql("SELECT 1")).await.unwrap();

        // close_by_ref 关的是共享的底层连接池，pool 本身还能用来发查询
        pool.inner.close_by_ref().await.unwrap();

        // 关闭后再发查询应该失败
        let result = pool.query_one_raw(sql("SELECT 1")).await;
        assert!(result.is_err(), "{result:?}");
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

    /// sqlx 往 timestamptz 列写入再读回，可空列写 NULL 读回 None；
    /// 库里按 +08:00 写的值读出来换算成 UTC
    #[tokio::test]
    async fn sqlx_hy_utc_datetime_round_trip() {
        let server = start().await;
        let pool = SqlxPgPool::connect(sqlx_config(&server)).await.unwrap();
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

    /// SeaORM 实体字段用 `HyUtcDateTime`，插入再按主键查回，可空字段 None 对应 NULL；
    /// 库里按 +08:00 写的值读出来换算成 UTC
    #[tokio::test]
    async fn seaorm_hy_utc_datetime_round_trip() {
        let server = start().await;
        let pool = SeaOrmPgPool::connect(seaorm_config(&server)).await.unwrap();
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
}
