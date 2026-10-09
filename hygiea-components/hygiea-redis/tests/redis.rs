//! Redis 连接池连真实服务，`#[container]` 层（见仓库根目录 AGENTS.md 的测试分层）：
//! 用代码启动官方 redis 容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）
//! ```sh
//! cargo test -p hygiea-redis --features fred --test redis -- --ignored ::container::
//! ```

#![cfg(feature = "fred")]

#[hygiea_test::container]
mod redis {
    use fred::interfaces::KeysInterface;
    use hygiea_core::app::{ConfigResource, Registry, Resources};
    use hygiea_redis::{FredRedisPool, RedisComponent, RedisConfig};
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

    /// 经 Registry 具名启动 RedisComponent：连接池放在组件名下，取出来能 SET/GET
    #[tokio::test]
    async fn component_starts_and_provides_named_pool() {
        let redis = start().await;
        let registry = Registry::new().add_named::<RedisComponent>("cache", config(&redis, 0));
        let pool = with_resources(registry, |res| res.get_named::<FredRedisPool>("cache"))
            .await
            .expect("组件名下应该有连接池");

        let () = pool
            .set("component_key", "component_value", None, None, false)
            .await
            .unwrap();
        let value: String = pool.get("component_key").await.unwrap();
        assert_eq!(value, "component_value");
    }

    #[tokio::test]
    async fn connect_to_redis_success() {
        let redis = start().await;
        FredRedisPool::from_config(&config(&redis, 0))
            .await
            .expect("应该能连接到 Redis");
    }

    #[tokio::test]
    async fn set_and_get_value() {
        let redis = start().await;
        let pool = FredRedisPool::from_config(&config(&redis, 0))
            .await
            .unwrap();

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
        let pool = FredRedisPool::from_config(&config(&redis, 1))
            .await
            .unwrap();

        // 在 db 1 里 SET 一个值，能 GET 出来
        let () = pool
            .set("db1_key", "db1_value", None, None, false)
            .await
            .unwrap();
        let value: String = pool.get("db1_key").await.unwrap();
        assert_eq!(value, "db1_value");
    }
}

// ---- 临时探针：每个只多一样东西，看完删掉 ----

/// 只多一个生命周期 'static
#[hygiea_test::container]
mod probe_lifetime {
    fn helper(s: &'static str) -> &'static str {
        s
    }

    #[test]
    fn lifetime() {
        let _ = helper("a");
    }
}

/// 只多泛型（不带生命周期）
#[hygiea_test::container]
mod probe_generic {
    fn helper<T: Send>(t: T) -> T {
        t
    }

    #[test]
    fn generic() {
        let _ = helper(1);
    }
}

/// 只多 impl FnOnce 参数
#[hygiea_test::container]
mod probe_impl_fn {
    fn helper(f: impl FnOnce() -> i32) -> i32 {
        f()
    }

    #[test]
    fn impl_fn() {
        let _ = helper(|| 1);
    }
}

/// 只多一个 async 辅助函数
#[hygiea_test::container]
mod probe_async {
    async fn helper() -> i32 {
        1
    }

    #[tokio::test]
    async fn async_helper() {
        let _ = helper().await;
    }
}

/// probe_async 的完全副本：结果和 probe_async 不一样，就是 RustRover 本身不稳定
#[hygiea_test::container]
mod probe_async_copy {


    async fn helper() -> i32 {
        1
    }

    #[tokio::test]
    async fn async_helper() {
        let _ = helper().await;
    }
}

/// async 辅助函数 + 同步的 #[test]：看是 async 辅助函数本身，还是它和 #[tokio::test] 一起出现才有问题
#[hygiea_test::container]
mod probe_async_sync_test {
    #[allow(dead_code)]
    async fn helper() -> i32 {
        1
    }

    #[test]
    fn sync_test() {}
}

/// 不挂宏，async 辅助函数 + #[tokio::test]：没按钮就跟我们的宏无关
mod probe_async_no_macro {
    async fn helper() -> i32 {
        1
    }

    #[tokio::test]
    async fn async_helper() {
        let _ = helper().await;
    }
}
