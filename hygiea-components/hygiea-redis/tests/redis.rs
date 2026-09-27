//! Redis 连接池集成测试
//!
//! 需要运行 Redis 服务。手动运行方式：
//! ```sh
//! # 需要环境变量
//! export REDIS_HOST=localhost
//! export REDIS_PORT=6379
//!
//! cargo test -p hygiea-redis --all-features --test redis -- --ignored --nocapture
//! ```

#![cfg(feature = "fred")]

use fred::interfaces::KeysInterface;
use hygiea_core::app::{Component, Name};
use hygiea_redis::{RedisComponent, RedisConfig};

fn get_redis_config() -> (String, u16) {
    let host = std::env::var("REDIS_HOST").unwrap_or("localhost".to_string());
    let port = std::env::var("REDIS_PORT")
        .unwrap_or("6379".to_string())
        .parse()
        .unwrap_or(6379);
    (host, port)
}

#[tokio::test]
#[ignore = "requires running Redis"]
async fn connect_to_redis_success() {
    let (host, port) = get_redis_config();
    let config = RedisConfig {
        mode: "standalone".to_string(),
        host,
        port,
        ..Default::default()
    };

    let pool = hygiea_redis::FredRedisPool::connect(config).await;
    assert!(pool.is_ok(), "应该能连接到 Redis");
}

#[tokio::test]
#[ignore = "requires running Redis"]
async fn set_and_get_value() {
    let (host, port) = get_redis_config();
    let config = RedisConfig {
        mode: "standalone".to_string(),
        host,
        port,
        db: 0,
        ..Default::default()
    };

    let pool = hygiea_redis::FredRedisPool::connect(config)
        .await
        .expect("应该能连接到 Redis");

    // SET 一个值
    let set_result: Result<(), _> = pool.set("test_key", "test_value", None, None, false).await;
    assert!(set_result.is_ok(), "SET 命令应该成功");

    // GET 取出来验证
    let get_result: Result<String, _> = pool.get("test_key").await;
    assert!(get_result.is_ok(), "GET 命令应该成功");
    assert_eq!(get_result.unwrap(), "test_value");
}

#[test]
fn component_provides_correct_resource() {
    // 测试组件声明的资源（不需要 Redis 运行）
    let name = Name::from("cache");
    let component = RedisComponent::build(name.clone(), RedisConfig::default());

    let provides = component.provides();
    assert_eq!(provides.len(), 1, "应该提供一个资源");
    let id_string = provides[0].to_string();
    assert!(
        id_string.contains(&name.to_string()),
        "ResourceId 应该包含组件名 '{}', 得到: {}",
        name,
        id_string
    );
}

#[tokio::test]
#[ignore = "requires running Redis"]
async fn select_database() {
    let (host, port) = get_redis_config();
    let config = RedisConfig {
        mode: "standalone".to_string(),
        host,
        port,
        db: 1,
        ..Default::default()
    };

    let pool = hygiea_redis::FredRedisPool::connect(config)
        .await
        .expect("应该能连接到 Redis");

    // 在 db 1 里 SET 一个值
    let set_result: Result<(), _> = pool.set("db1_key", "db1_value", None, None, false).await;
    assert!(set_result.is_ok());

    // 能 GET 出来
    let get_result: Result<String, _> = pool.get("db1_key").await;
    assert!(get_result.is_ok());
    assert_eq!(get_result.unwrap(), "db1_value");
}
