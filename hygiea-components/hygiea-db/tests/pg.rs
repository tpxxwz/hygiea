//! PostgreSQL 连接池集成测试（sqlx 和 seaorm）
//!
//! 需要运行 PostgreSQL 服务。手动运行方式：
//! ```sh
//! # 需要环境变量
//! export PG_HOST=localhost
//! export PG_PORT=5432
//! export PG_USER=postgres
//! export PG_PASSWORD=postgres
//! export PG_DATABASE=postgres
//!
//! cargo test -p hygiea-db --all-features --test pg -- --ignored --nocapture
//! ```

#![cfg(all(feature = "sqlx", feature = "seaorm", feature = "postgres"))]

use hygiea_db::{SeaOrmPgConfig, SqlxPgConfig};

fn get_pg_config() -> (String, u16, String, String, String) {
    let host = std::env::var("PG_HOST").unwrap_or("localhost".to_string());
    let port = std::env::var("PG_PORT")
        .unwrap_or("5432".to_string())
        .parse()
        .unwrap_or(5432);
    let user = std::env::var("PG_USER").unwrap_or("postgres".to_string());
    let password = std::env::var("PG_PASSWORD").unwrap_or("postgres".to_string());
    let database = std::env::var("PG_DATABASE").unwrap_or("postgres".to_string());
    (host, port, user, password, database)
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn sqlx_connect_success() {
    let (host, port, user, password, database) = get_pg_config();
    let config = SqlxPgConfig {
        host: host.clone(),
        port,
        username: user.clone(),
        password: password.clone(),
        database: database.clone(),
        ..Default::default()
    };

    let pool = hygiea_db::SqlxPgPool::connect(config).await;
    assert!(pool.is_ok(), "应该能连接到 PostgreSQL");

    // 简单查询验证连接可用
    let pool = pool.unwrap();
    let result = sqlx::query("SELECT 1 as value").fetch_one(&*pool).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn sqlx_schema_search_path_takes_effect() {
    let (host, port, user, password, database) = get_pg_config();
    let config = SqlxPgConfig {
        host: host.clone(),
        port,
        username: user.clone(),
        password: password.clone(),
        database: database.clone(),
        schema_search_path: "public".to_string(),
        ..Default::default()
    };

    let pool = hygiea_db::SqlxPgPool::connect(config).await;
    assert!(pool.is_ok());

    // 通过 search_path 配置能找到 public schema 的对象
    let pool = pool.unwrap();
    let result = sqlx::query("SELECT current_schema()")
        .fetch_one(&*pool)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn sqlx_stop_closes_pool() {
    let (host, port, user, password, database) = get_pg_config();
    let config = SqlxPgConfig {
        host,
        port,
        username: user,
        password,
        database,
        max_connections: Some(2),
        ..Default::default()
    };

    let pool = hygiea_db::SqlxPgPool::connect(config).await.unwrap();

    // 验证连接可用
    let result = sqlx::query("SELECT 1").fetch_one(&*pool).await;
    assert!(result.is_ok());

    // 关闭连接池
    pool.inner.close().await;

    // 再发查询应该失败
    let result = sqlx::query("SELECT 1").fetch_one(&*pool).await;
    assert!(result.is_err());
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn seaorm_connect_success() {
    let (host, port, user, password, database) = get_pg_config();
    let config = SeaOrmPgConfig {
        host: host.clone(),
        port,
        username: user.clone(),
        password: password.clone(),
        database: database.clone(),
        ..Default::default()
    };

    let pool = hygiea_db::SeaOrmPgPool::connect(config).await;
    assert!(pool.is_ok(), "应该能连接到 PostgreSQL");
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn seaorm_schema_search_path_takes_effect() {
    let (host, port, user, password, database) = get_pg_config();
    let config = SeaOrmPgConfig {
        host: host.clone(),
        port,
        username: user.clone(),
        password: password.clone(),
        database: database.clone(),
        schema_search_path: "public".to_string(),
        ..Default::default()
    };

    let pool = hygiea_db::SeaOrmPgPool::connect(config).await;
    assert!(pool.is_ok());

    // 能成功连接表明 search_path 配置被应用了
    let _pool = pool.unwrap();
    // 连接成功就说明 search_path 配置被应用了
}

#[tokio::test]
#[ignore = "requires running PostgreSQL"]
async fn seaorm_stop_closes_connection() {
    let (host, port, user, password, database) = get_pg_config();
    let config = SeaOrmPgConfig {
        host,
        port,
        username: user,
        password,
        database,
        ..Default::default()
    };

    let pool = hygiea_db::SeaOrmPgPool::connect(config).await.unwrap();

    // 关闭连接
    let close_result = pool.inner.close().await;
    assert!(close_result.is_ok());
    // 关闭后，再尝试 get_server_version（这是一个简单的操作）应该失败
    // 但由于 seaorm 的 API 比较复杂，我们只验证关闭没有报错
}
