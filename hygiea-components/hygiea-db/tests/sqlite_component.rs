//! SQLite 组件集成测试，使用内存数据库
//!
//! 运行：`cargo test -p hygiea-db --all-features --test sqlite_component`

#![cfg(all(feature = "sqlx", feature = "sqlite"))]

use hygiea_core::app::{CancellationToken, Component, Name, Registry};
use hygiea_db::{SqlxSqliteComponent, SqlxSqliteConfig, SqlxSqlitePool};
use std::sync::{Arc, Mutex};

#[test]
fn provides_returns_resource_with_component_name() {
    // 测试 provides 返回的 ResourceId 包含组件名
    let name = Name::from("my_sqlite_db");
    let component = SqlxSqliteComponent::build(
        name.clone(),
        hygiea_db::SqlxSqliteConfig {
            database: ":memory:".to_string(),
            ..Default::default()
        },
    );

    let provides = component.provides();
    assert_eq!(provides.len(), 1);
    // ResourceId 的 Display 格式是 "TypeName(name)"
    let id_string = provides[0].to_string();
    assert!(
        id_string.contains(&name.to_string()),
        "ResourceId should contain component name '{}', got: {}",
        name,
        id_string
    );
}

#[tokio::test]
async fn stop_twice_does_not_error() {
    // 测试 stop 两次都成功（第一次关闭，第二次是空操作）
    let name = Name::from("test_pool");
    let mut component = SqlxSqliteComponent::build(
        name,
        hygiea_db::SqlxSqliteConfig {
            database: ":memory:".to_string(),
            ..Default::default()
        },
    );

    // 创建一个虚拟的 shutdown token（不用，直接测试 stop）
    let _shutdown = CancellationToken::new();

    // 直接调用 stop（没有 startup）
    let result1 = component.stop().await;
    assert!(result1.is_ok(), "First stop should succeed");

    // 第二次 stop 也应该成功（因为 pool 已经被 take 出来了，第二次是空操作）
    let result2 = component.stop().await;
    assert!(result2.is_ok(), "Second stop should also succeed");
}

#[tokio::test]
async fn connect_to_file_database() {
    // 测试直接连接文件数据库
    let tmpdir = tempfile::TempDir::new().expect("Should create temp dir");
    let db_path = tmpdir.path().join("test.db");

    let config = hygiea_db::SqlxSqliteConfig {
        database: db_path.to_string_lossy().to_string(),
        create_if_missing: true,
        ..Default::default()
    };

    let pool = hygiea_db::SqlxSqlitePool::connect(config)
        .await
        .expect("Should connect successfully");

    // 创建表
    sqlx::query("CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT)")
        .execute(&*pool)
        .await
        .expect("Should create table");

    // 插入数据
    sqlx::query("INSERT INTO test (value) VALUES (?)")
        .bind("test_value")
        .execute(&*pool)
        .await
        .expect("Should insert data");

    // 查询数据
    let (value,): (String,) = sqlx::query_as("SELECT value FROM test WHERE id = 1")
        .fetch_one(&*pool)
        .await
        .expect("Should fetch data");

    assert_eq!(value, "test_value");
}

// 要给自己发 SIGTERM 触发 Registry 的优雅关闭，只在 unix 上跑
#[cfg(unix)]
#[tokio::test]
async fn registry_startup_and_stop_with_primary_and_replica() {
    // 测试通过 Registry 注册 primary 和 replica 两个实例：
    // - startup 后能 get_named 取到连接池
    // - stop 后连接池已关闭
    // - 两个实例互不相同

    let memory_config = || SqlxSqliteConfig {
        database: ":memory:".to_string(),
        ..Default::default()
    };

    let registry = Registry::new()
        .add_named::<SqlxSqliteComponent>("primary", memory_config())
        .add_named::<SqlxSqliteComponent>("replica", memory_config());

    // 用 Arc<Mutex<>> 来在异步闭包内外传递值
    let pools_holder: Arc<Mutex<Option<(SqlxSqlitePool, SqlxSqlitePool)>>> =
        Arc::new(Mutex::new(None));
    let pools_holder_clone = Arc::clone(&pools_holder);

    // 运行时的实际行为：回调执行完后，run 会等待退出信号
    // 我们用一个后台任务发送信号来退出测试
    let run_future = registry
        .on_ready(move |state| {
            let holder = Arc::clone(&pools_holder_clone);
            async move {
                // 通过 get_named 获取两个连接池
                let primary = state
                    .get_named::<SqlxSqlitePool>("primary")
                    .expect("primary pool should exist");
                let replica = state
                    .get_named::<SqlxSqlitePool>("replica")
                    .expect("replica pool should exist");

                // 验证两个连接池都可用（执行查询）
                let _: (i32,) = sqlx::query_as("SELECT 1")
                    .fetch_one(&*primary)
                    .await
                    .expect("primary pool should be usable");

                let _: (i32,) = sqlx::query_as("SELECT 1")
                    .fetch_one(&*replica)
                    .await
                    .expect("replica pool should be usable");

                // 克隆连接池到外面，带出回调
                *holder.lock().unwrap() = Some((primary.clone(), replica.clone()));

                Ok(())
            }
        })
        .run();

    // 在后台发送 SIGTERM 给自己来优雅退出 run()
    // 这会导致 Registry 正常关闭所有组件
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // 给自己发 SIGTERM，用系统的 kill 命令，不额外引入依赖
        std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .unwrap();
    });

    let (run_result, _log_guard) = run_future.await;

    // SIGTERM 会导致正常关闭，返回 Ok
    assert!(
        run_result.is_ok(),
        "Registry should shutdown gracefully on SIGTERM"
    );

    // 验证克隆出来的连接池已关闭
    assert!(
        pools_holder.lock().unwrap().is_some(),
        "pools should be set"
    );

    let (primary, replica) = pools_holder
        .lock()
        .unwrap()
        .take()
        .expect("pools should be set");

    // 关闭后执行查询应该失败
    let primary_query = sqlx::query("SELECT 1").fetch_one(&*primary).await;
    assert!(
        primary_query.is_err(),
        "primary pool should be closed after graceful shutdown"
    );

    let replica_query = sqlx::query("SELECT 1").fetch_one(&*replica).await;
    assert!(
        replica_query.is_err(),
        "replica pool should be closed after graceful shutdown"
    );
}
