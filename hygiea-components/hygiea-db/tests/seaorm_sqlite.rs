//! `SeaOrmSqlitePool`（`src/seaorm/pool/sqlite.rs`，feature `seaorm` + `sqlite`）的集成测试，feature 全开（`live-config`）。
//! SQLite 嵌在进程里，不需要 Docker，默认的 `cargo test` 就会跑：
//! ```sh
//! cargo test -p hygiea-db --features seaorm,sqlite,live-config --test seaorm_sqlite
//! ```
//! 动态配置和 `SqlxSqlitePool` 是同一份实现，完整的测试在 `sqlx_sqlite.rs`；这里确认 SeaORM 的连接池接上了，
//! `[db.connect]` 里的打开设置（SeaORM 只收 URL，靠回调加上）真的生效

#![cfg(all(feature = "seaorm", feature = "sqlite", feature = "live-config"))]

mod common;

use common::eventually;
use hygiea_core::app::{ConfigResource, RegistryConfig};
use hygiea_db::{PoolConfig, SeaOrmSqliteConfig, SeaOrmSqlitePool, SqliteConnectConfig};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Cache {
    ttl_secs: u64,
}

/// 应用名 `hy`：动态配置的表是 hy_config 那三张
fn app() -> RegistryConfig {
    RegistryConfig {
        app_name: "hy".to_string(),
        ..Default::default()
    }
}

fn file_config(dir: &tempfile::TempDir) -> SeaOrmSqliteConfig {
    SeaOrmSqliteConfig {
        connect: SqliteConnectConfig {
            database: dir.path().join("seaorm.db").display().to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn pragma(pool: &SeaOrmSqlitePool, name: &str) -> String {
    pool.query_one_raw(Statement::from_string(
        DbBackend::Sqlite,
        format!("SELECT CAST({name} AS TEXT) FROM pragma_{name}"),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get_by_index::<String>(0)
    .unwrap()
}

/// 连接池本身：打开设置、经 Registry 启动、实体里的 HyUtcDateTime、seaorm::transaction
mod pool {
    use hygiea_core::app::{Registry, Resources};
    use hygiea_core::datetime::{HygieaDateTimeExt, UtcDateTime};
    use hygiea_core::{ResultExt, err};
    use hygiea_db::{HyUtcDateTime, RepoErr, SeaOrmSqliteComponent};
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

    /// `[db.connect]` 的打开设置（默认 journal_mode = wal、foreign_keys = true、synchronous = full）在每条连接上生效，
    /// 改了的按改的
    #[tokio::test]
    async fn connect_settings_take_effect() {
        let dir = tempfile::tempdir().unwrap();
        let pool = SeaOrmSqlitePool::from_config(&file_config(&dir), &app())
            .await
            .unwrap();
        assert_eq!(pragma(&pool, "journal_mode").await, "wal");
        assert_eq!(pragma(&pool, "foreign_keys").await, "1");
        assert_eq!(pragma(&pool, "synchronous").await, "2", "full");
        pool.close().await.unwrap();

        let mut config = file_config(&dir);
        config.connect.journal_mode = Some("delete".to_string());
        config.connect.foreign_keys = false;
        let pool = SeaOrmSqlitePool::from_config(&config, &app())
            .await
            .unwrap();
        assert_eq!(pragma(&pool, "journal_mode").await, "delete");
        assert_eq!(pragma(&pool, "foreign_keys").await, "0");
    }

    /// 经 Registry 启动，按组件名取到能用的连接（内存库，池限成 1 条连接）；实体字段 HyUtcDateTime 插入再查回；
    /// seaorm::transaction 闭包返回 Err 时整个回滚
    #[tokio::test]
    async fn registry_entity_transaction() {
        let config = SeaOrmSqliteConfig {
            connect: SqliteConnectConfig {
                database: ":memory:".to_string(),
                ..Default::default()
            },
            pool: PoolConfig {
                max_connections: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let registry = Registry::new().add_named::<SeaOrmSqliteComponent>("primary", config);
        let pool = with_resources(registry, |res| res.get_named::<SeaOrmSqlitePool>("primary"))
            .await
            .unwrap();
        pool.execute_unprepared(
            "CREATE TABLE record (id INTEGER PRIMARY KEY, create_at TEXT NOT NULL, delete_at TEXT)",
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
                    "INSERT INTO record (id, create_at) VALUES ({id}, '2026-01-01T00:00:00Z')"
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

/// 动态配置：小池另开一条连接，内存库连到的是另一个空库，所以用本地临时文件
mod live_config {
    use hygiea_db::SqliteLiveConfigConfig;

    use super::*;

    #[tokio::test]
    async fn update_is_polled() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = file_config(&dir);
        config.live_config = SqliteLiveConfigConfig {
            enabled: true,
            ..Default::default()
        };
        let pool = SeaOrmSqlitePool::from_config(&config, &app())
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
}
