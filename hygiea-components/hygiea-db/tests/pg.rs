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
    use hygiea_db::{SeaOrmPgConfig, SeaOrmPgPool, SqlxPgConfig, SqlxPgPool};
    use hygiea_test::container::{ContainerSpec, RunningContainer};
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

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
}
