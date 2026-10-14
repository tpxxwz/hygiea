//! `SeaOrmMySqlPool`（`src/seaorm/pool/mysql.rs`，feature `seaorm` + `mysql`）的集成测试，feature 全开（`lock`、`live-config`），container 层：
//! 用代码启动官方 mysql 容器，需要 Docker（Podman 把 `DOCKER_HOST` 指向它的兼容 socket）
//! ```sh
//! cargo test -p hygiea-db --features seaorm,mysql,lock,live-config --test seaorm_mysql -- --ignored ::container::
//! ```
//! 锁、动态配置和 `SqlxMySqlPool` 是同一份实现，完整的测试在 `sqlx_mysql.rs`；这里确认 SeaORM 的连接池接上了

#![cfg(all(
    feature = "seaorm",
    feature = "mysql",
    feature = "lock",
    feature = "live-config"
))]

mod common;

use std::time::Duration;

use common::eventually;
use hygiea_core::app::{ConfigResource, RegistryConfig};
use hygiea_core::lock::{hy_lock_key, lock_key};
use hygiea_db::{MySqlConnectConfig, SeaOrmMySqlConfig, SeaOrmMySqlPool};
use hygiea_test::container;
use hygiea_test::container::{ContainerSpec, RunningContainer};
use sea_orm::ConnectionTrait;
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

const PASSWORD: &str = "hygiea-test";

const DATABASE: &str = "hy";

const PORT: u16 = 3306;

/// 镜像固定版本（8.x 的 LTS）。启动时先起临时实例（`port: 0`）做初始化再重启，就绪日志出现两次，第二次才是正式实例
async fn start() -> RunningContainer {
    ContainerSpec::new("mysql", "8.4")
        .env("MYSQL_ROOT_PASSWORD", PASSWORD)
        .env("MYSQL_DATABASE", DATABASE)
        .port(PORT)
        .wait_log_times("mysqld: ready for connections", 2)
        .startup_timeout(Duration::from_secs(180))
        .start()
        .await
}

/// 应用名 `hy`：动态配置的表是 hy_config 那三张
fn app() -> RegistryConfig {
    RegistryConfig {
        app_name: "hy".to_string(),
        ..Default::default()
    }
}

fn config(server: &RunningContainer) -> SeaOrmMySqlConfig {
    SeaOrmMySqlConfig {
        connect: MySqlConnectConfig {
            host: server.host().to_string(),
            port: server.port(PORT),
            password: PASSWORD.to_string(),
            database: DATABASE.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn pool(server: &RunningContainer) -> SeaOrmMySqlPool {
    SeaOrmMySqlPool::from_config(&config(server), &app())
        .await
        .unwrap()
}

/// 连接池本身：经 Registry 启动、实体里的 HyUtcDateTime、seaorm::transaction
#[container]
mod pool {
    use hygiea_core::app::{Registry, Resources};
    use hygiea_core::datetime::{HygieaDateTimeExt, UtcDateTime};
    use hygiea_core::{ResultExt, err};
    use hygiea_db::{HyUtcDateTime, RepoErr, SeaOrmMySqlComponent};
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};

    use super::*;

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

    /// 经 Registry 启动，按组件名取到能用的连接；实体字段 HyUtcDateTime 插入再查回，None 对应 NULL；
    /// seaorm::transaction 闭包返回 Err 时整个回滚
    #[tokio::test]
    async fn registry_entity_transaction() {
        let server = start().await;
        let registry =
            Registry::new().add_named::<SeaOrmMySqlComponent>("primary", config(&server));
        let pool = with_resources(registry, |res| res.get_named::<SeaOrmMySqlPool>("primary"))
            .await
            .unwrap();
        pool.execute_unprepared(
            "CREATE TABLE record (id BIGINT PRIMARY KEY, create_at TIMESTAMP(6) NOT NULL, \
             delete_at TIMESTAMP(6) NULL)",
        )
        .await
        .unwrap();

        let at = HyUtcDateTime(UtcDateTime::parse_ext_rfc3339("2026-10-05T12:57:24.719Z").unwrap());
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
        let mut rows: Vec<_> = record::Entity::find()
            .all(&*pool)
            .await
            .unwrap()
            .into_iter()
            .map(|m| (m.id, m.create_at, m.delete_at))
            .collect();
        rows.sort_by_key(|r| r.0);
        assert_eq!(rows, vec![(1, at, Some(at)), (2, at, None)]);

        // 第二条主键冲突：整个事务回滚，第一条也不留
        let e = hygiea_db::seaorm::transaction(&*pool, "save record", async |txn| {
            for id in [3, 1] {
                txn.execute_unprepared(&format!(
                    "INSERT INTO record (id, create_at) VALUES ({id}, NOW())"
                ))
                .await
                .wrap_err(|| err!(RepoErr::InsertFailed, "insert record"))?;
            }
            Ok(())
        })
        .await
        .unwrap_err();
        assert!(e.is(RepoErr::InsertFailed), "{e:#}");
        assert_eq!(record::Entity::find().all(&*pool).await.unwrap().len(), 2);
    }
}

/// 锁：用 SeaORM 底下的 sqlx 池
#[container]
mod lock {
    use hygiea_core::lock::DistributedLock;

    use super::*;

    #[tokio::test]
    async fn i64_and_string_key_are_exclusive() {
        let server = start().await;
        let (a, b) = (pool(&server).await, pool(&server).await);
        let guard = a.try_lock_i64(1).await.unwrap().unwrap();
        assert_eq!(guard.key(), "i64:1");
        assert!(b.try_lock_i64(1).await.unwrap().is_none());
        guard.release().await.unwrap();
        assert!(b.try_lock_i64(1).await.unwrap().is_some());

        let key = || lock_key!(TestLockKey::Job, 1);
        let guard = a.try_lock(key()).await.unwrap().unwrap();
        assert!(
            b.lock(key(), Duration::from_secs(1))
                .await
                .unwrap()
                .is_none()
        );
        guard.release().await.unwrap();
        assert!(
            b.lock(key(), Duration::from_secs(5))
                .await
                .unwrap()
                .is_some()
        );
    }
}

/// 动态配置：SeaORM 连接池交出连接参数，另开小池，只靠轮询
#[container]
mod live_config {
    use hygiea_db::MySqlLiveConfigConfig;

    use super::*;

    #[tokio::test]
    async fn update_is_polled() {
        let server = start().await;
        let mut config = config(&server);
        config.live_config = MySqlLiveConfigConfig {
            enabled: true,
            poll_interval_secs: Some(1),
            ..Default::default()
        };
        let pool = SeaOrmMySqlPool::from_config(&config, &app()).await.unwrap();
        let cache = pool.live_config("cache", Cache::default()).await.unwrap();
        assert_eq!(cache.get().ttl_secs, 0);
        pool.execute_unprepared("UPDATE hy_config SET value = '8' WHERE namespace = 'cache'")
            .await
            .unwrap();
        eventually(5, || async { cache.get().ttl_secs == 8 }).await;
        pool.close().await.unwrap();
    }
}
