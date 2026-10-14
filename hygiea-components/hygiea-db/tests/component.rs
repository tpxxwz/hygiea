//! 各数据库组件：构造后 `provides` 声明按组件名的连接池；SQLite 再经 Registry 用内存库真正启动。
//! PostgreSQL 的两个组件依赖外部服务，这里只测构造和 `provides`，经 Registry 启动的测试在 `tests/pg.rs`
//!
//! 运行：`cargo test -p hygiea-db --all-features --test component`

#![cfg(any(
    all(feature = "sqlx", feature = "sqlite"),
    all(feature = "sqlx", feature = "postgres"),
    all(feature = "seaorm", feature = "postgres"),
    all(feature = "sqlx", feature = "mysql"),
    all(feature = "tokio-postgres", feature = "postgres"),
    all(feature = "toasty", feature = "postgres"),
    all(feature = "toasty", feature = "mysql")
))]

#[cfg(all(feature = "sqlx", feature = "sqlite"))]
mod sqlite {
    use hygiea_core::app::{Component, Name, Registry, ResourceId, Resources};
    use hygiea_db::{SqliteConnectConfig, SqlxSqliteComponent, SqlxSqliteConfig, SqlxSqlitePool};

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

    /// 按组件名提供连接池
    #[test]
    fn provides_named_pool() {
        let component = SqlxSqliteComponent::build(Name::from("primary"), memory_config());
        assert_eq!(
            component.provides(),
            vec![ResourceId::named::<SqlxSqlitePool>("primary")]
        );
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
}

#[cfg(all(feature = "sqlx", feature = "postgres"))]
mod sqlx_pg {
    use hygiea_core::app::{Component, Name, ResourceId};
    use hygiea_db::{SqlxPgComponent, SqlxPgConfig, SqlxPgPool};

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        let named = SqlxPgComponent::build(Name::from("primary"), SqlxPgConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<SqlxPgPool>("primary")]
        );

        let anonymous = SqlxPgComponent::build(Name::from(""), SqlxPgConfig::default());
        assert_eq!(anonymous.provides(), vec![ResourceId::of::<SqlxPgPool>()]);
    }
}

#[cfg(all(feature = "seaorm", feature = "postgres"))]
mod seaorm_pg {
    use hygiea_core::app::{Component, Name, ResourceId};
    use hygiea_db::{SeaOrmPgComponent, SeaOrmPgConfig, SeaOrmPgPool};

    /// 按组件名提供连接，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        let named = SeaOrmPgComponent::build(Name::from("primary"), SeaOrmPgConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<SeaOrmPgPool>("primary")]
        );

        let anonymous = SeaOrmPgComponent::build(Name::from(""), SeaOrmPgConfig::default());
        assert_eq!(anonymous.provides(), vec![ResourceId::of::<SeaOrmPgPool>()]);
    }
}

#[cfg(all(feature = "toasty", feature = "postgres"))]
mod toasty_pg {
    use hygiea_core::app::{Component, Name, ResourceId};
    use hygiea_db::{ToastyPgComponent, ToastyPgConfig, ToastyPgPool};

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        let named = ToastyPgComponent::build(Name::from("primary"), ToastyPgConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<ToastyPgPool>("primary")]
        );

        let anonymous = ToastyPgComponent::build(Name::from(""), ToastyPgConfig::default());
        assert_eq!(anonymous.provides(), vec![ResourceId::of::<ToastyPgPool>()]);
    }

    /// 开了 lock：实现了 DistributedLock（编译期检查）
    #[cfg(feature = "lock")]
    #[test]
    fn implements_distributed_lock() {
        fn assert_lock<L: hygiea_core::lock::DistributedLock>() {}
        assert_lock::<ToastyPgPool>();
    }
}

#[cfg(all(feature = "toasty", feature = "mysql"))]
mod toasty_mysql {
    use hygiea_core::app::{Component, Name, ResourceId};
    use hygiea_db::{ToastyMySqlComponent, ToastyMySqlConfig, ToastyMySqlPool};

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        let named =
            ToastyMySqlComponent::build(Name::from("primary"), ToastyMySqlConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<ToastyMySqlPool>("primary")]
        );

        let anonymous = ToastyMySqlComponent::build(Name::from(""), ToastyMySqlConfig::default());
        assert_eq!(
            anonymous.provides(),
            vec![ResourceId::of::<ToastyMySqlPool>()]
        );
    }
}

#[cfg(all(feature = "sqlx", feature = "mysql"))]
mod sqlx_mysql {
    use hygiea_core::app::{Component, Name, ResourceId};
    use hygiea_db::{SqlxMySqlComponent, SqlxMySqlConfig, SqlxMySqlPool};

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        let named = SqlxMySqlComponent::build(Name::from("primary"), SqlxMySqlConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<SqlxMySqlPool>("primary")]
        );

        let anonymous = SqlxMySqlComponent::build(Name::from(""), SqlxMySqlConfig::default());
        assert_eq!(
            anonymous.provides(),
            vec![ResourceId::of::<SqlxMySqlPool>()]
        );
    }

    /// 开了 lock：实现了 DistributedLock（编译期检查）
    #[cfg(feature = "lock")]
    #[test]
    fn implements_distributed_lock() {
        fn assert_lock<L: hygiea_core::lock::DistributedLock>() {}
        assert_lock::<SqlxMySqlPool>();
    }
}

#[cfg(all(feature = "tokio-postgres", feature = "postgres"))]
mod tokio_pg {
    use hygiea_core::app::{Component, Name, ResourceId};
    use hygiea_db::{TokioPgComponent, TokioPgConfig, TokioPgPool};

    /// 按组件名提供连接池，匿名的名字是 `""`
    #[test]
    fn provides_named_pool() {
        let named = TokioPgComponent::build(Name::from("primary"), TokioPgConfig::default());
        assert_eq!(
            named.provides(),
            vec![ResourceId::named::<TokioPgPool>("primary")]
        );

        let anonymous = TokioPgComponent::build(Name::from(""), TokioPgConfig::default());
        assert_eq!(anonymous.provides(), vec![ResourceId::of::<TokioPgPool>()]);
    }

    /// 开了 lock：实现了 DistributedLock（编译期检查）
    #[cfg(feature = "lock")]
    #[test]
    fn implements_distributed_lock() {
        fn assert_lock<L: hygiea_core::lock::DistributedLock>() {}
        assert_lock::<TokioPgPool>();
    }
}
