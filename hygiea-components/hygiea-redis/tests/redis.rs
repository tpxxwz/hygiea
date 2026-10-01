//! Redis 连接池连真实服务，`#[container]` 层（见仓库根目录 AGENTS.md 的测试分层）：
//! 用代码启动官方 redis 容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）
//! ```sh
//! cargo test -p hygiea-redis --features fred --test redis -- --ignored ::container::
//! ```

#![cfg(feature = "fred")]

use hygiea_test::container;

#[container]
mod redis {
    use fred::interfaces::KeysInterface;
    use hygiea_redis::{FredRedisPool, RedisConfig};
    use hygiea_test::container::{ContainerSpec, RunningContainer};

    const PORT: u16 = 6379;

    /// 镜像固定版本，日志出现 "Ready to accept connections" 算就绪
    async fn start() -> RunningContainer {
        ContainerSpec::new("redis", "7.4")
            .port(PORT)
            .wait_log("Ready to accept connections")
            .start()
            .await
    }

    fn config(redis: &RunningContainer, db: u8) -> RedisConfig {
        RedisConfig {
            mode: "standalone".to_string(),
            host: redis.host().to_string(),
            port: redis.port(PORT),
            db,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn connect_to_redis_success() {
        let redis = start().await;
        FredRedisPool::connect(config(&redis, 0))
            .await
            .expect("应该能连接到 Redis");
    }

    #[tokio::test]
    async fn set_and_get_value() {
        let redis = start().await;
        let pool = FredRedisPool::connect(config(&redis, 0)).await.unwrap();

        let () = pool
            .set("test_key", "test_value", None, None, false)
            .await
            .unwrap();
        let value: String = pool.get("test_key").await.unwrap();
        assert_eq!(value, "test_value");
    }

    #[tokio::test]
    async fn select_database() {
        let redis = start().await;
        let pool = FredRedisPool::connect(config(&redis, 1)).await.unwrap();

        // 在 db 1 里 SET 一个值，能 GET 出来
        let () = pool
            .set("db1_key", "db1_value", None, None, false)
            .await
            .unwrap();
        let value: String = pool.get("db1_key").await.unwrap();
        assert_eq!(value, "db1_value");
    }
}
