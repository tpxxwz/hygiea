//! 连接池的分布式锁（feature `lock`）：PG 的连接池直接实现 hygiea-core 的 [`DistributedLock`](hygiea_core::lock::DistributedLock)，
//! 用法和 key 的规则见 `hygiea::lock`。
//!
//! | 后端 | `try_lock_i64` / `try_lock_i32` 和阻塞版 | 字符串 key（`try_lock` / `lock`） |
//! |---|---|---|
//! | PostgreSQL（`SqlxPgPool`、`SeaOrmPgPool`） | 原生事务级 advisory lock | 开了 `lock-row`：在 `{p}lock` 表（前缀默认是应用名加 `_`，见 [`LockConfig`]）里插入不提交的行，靠主键冲突互斥，精确；没开：哈希成 i64 走 advisory lock，有损（见下） |
//!
//! PG 每次加锁开一个事务（占一条连接），锁交给返回的 guard：`release()` 或 drop 时回滚事务，锁跟着释放；
//! 进程崩溃、连接断开时 PG 中止事务，锁也释放。持锁期间那条连接一直占着。
//!
//! 连接从哪来看 [`LockConfig`]（`[db.lock]` 段）：`connect`、`max_connections` 都没写时用连接池本身，同时持有的锁加上
//! 业务查询要在连接池的 `max_connections` 以内；写了任意一项就单独建一个锁用的池（连到 `connect` 合并后的库、
//! 最多 `max_connections` 条），不和业务抢连接。
//!
//! - `try_lock_i32` 用原生的 `(int, int)` 形式：PG 上它和 `try_lock_i64` 是两个 key 空间（`(0, 42)` 和 `42` 是两把锁），
//!   和 hygiea-core 的默认实现（合成一个 i64）不同
//! - 阻塞版用 `lock_timeout` 原生等待，超时返回 `Ok(None)`；`timeout` 为 0 等于 `try_*`。`timeout` 精度是毫秒，
//!   必须是整数毫秒（`1.9ms` 这种带零头的报错），在 1 毫秒到 `i32::MAX` 毫秒（约 24.8 天，`lock_timeout` 的上限）之间（0 除外），
//!   否则返回 `LockErr::InvalidTimeout`，开事务之前就报错，不发任何 SQL
//! - 锁表的建表规则和动态配置一样：表在、列对上且是 UNLOGGED 就用，不在就建，对不上拒绝启动。启动时建（`connect_lazy` 时第一次用字符串 key 时建）
//!
//! # 开了 `lock-row` 时的字符串 key：锁表
//!
//! 加锁是在事务里 `INSERT INTO {p}lock (key) VALUES ($1)`、不提交；别人插同一个 key 时撞主键，要等这个事务结束。
//! 释放就是回滚，行跟着撤掉，表一直是空的，不随 key 的个数增长。
//!
//! - 表是 UNLOGGED：插入不写 WAL；崩溃后被清空、不复制到从库，对一张永远是空的表没有影响。主从切换后新主库上是空表，直接能用
//! - INSERT 没有 `NOWAIT` / `SKIP LOCKED`，`try_lock` 用 1 毫秒的 `lock_timeout`（最小值）：没人持有时不等；
//!   有人持有时约 1 毫秒后返回 `Ok(None)`，所以 `try_lock` 不是立即返回，而是最多等约 1 毫秒。
//!   没拿到时 PG 服务端日志会记一条 `ERROR: canceling statement due to lock timeout`（默认 `log_min_error_statement`
//!   还会带上语句），高频 try 时注意日志量
//! - 持有中的行没提交，别的会话查表看不到，查"谁持有某个 key"只能从 `pg_locks` 的 transactionid 等待关系反推
//! - 每次加锁留下一个回滚掉的元组和索引项，由 autovacuum 清理
//! - 不要手动往表里插入并提交：撞上已提交的行时 INSERT 立即报唯一约束冲突，这个 key 一直拿不到锁，返回 `LockErr::AcquireFailed`
//!
//! # 没开 `lock-row` 时的字符串 key：哈希降级
//!
//! 字符串 key 用 [`LockKey::hashed_i64`](hygiea_core::lock::LockKey::hashed_i64)（xxh3-64）转成 i64，走和 `try_lock_i64` 一样的
//! advisory lock，不建表。guard 的 key 显示成 `advisory <数字> (hashed <原 key>)`。要清楚这几点：
//!
//! - 有损：不同 key 可能撞到同一个数，撞上的两个 key 互相多锁（不会少锁）。100 万个 key 里有一对碰撞的概率约 3 × 10⁻⁸
//! - 和直接传数字的 `try_lock_i64` 共用一个空间：哈希值正好等于别处传的数字时也是同一把锁
//! - **开不开 `lock-row`，同一个 key 锁的是两样东西**（锁表的行 / 哈希出来的 advisory lock），互不互斥。
//!   所有实例的 feature 要一致；切换 `lock-row`（比如滚动发布时新版本打开了它）的窗口里，新旧实例对同一个 key 不互斥，
//!   要停机切换或者保证切换期间不会同时加锁
//! - feature 在整个依赖图里合并：任何一个依赖开了 `hygiea-db/lock-row`，这里就是锁表实现。用 `cargo tree -e features`
//!   确认实际开了哪个

use serde::Deserialize;

#[cfg(lock_pg)]
use crate::config::PgConnectOverride;

/// 连接池配置里的 `lock` 段（PG 连接池，sqlx、SeaORM）。各项都不写时锁用连接池本身
#[cfg(lock_pg)]
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LockConfig {
    /// 锁表（`lock-row`）的表名前缀：不写用应用名（`[registry] app_name`）加 `_`，写 `""` 不加前缀（表名是 `lock`），
    /// 写 `foo` 或 `foo_` 都是 `foo_lock`。去掉末尾的 `_` 后得是小写下划线 `[a-z][a-z0-9_]*`。没开 `lock-row` 时不用
    pub table_prefix: Option<String>,
    /// 锁用的池最多几条连接（同时持有、等待的锁各占一条）。写了就单独建池，不写时沿用连接池的 `max_connections`
    pub max_connections: Option<u32>,
    /// 单独连的库：`[db.lock.connect]`，只写和连接池的 `[db.connect]` 不一样的项，不写的沿用。写了任意一项就单独建池
    pub connect: PgConnectOverride,
}

// 规则（和数据库、驱动无关）。SQL 在 `sql/<数据库>/lock.rs`
#[cfg(lock_row_pg)]
mod table;
// 执行：每个驱动一个目录，里面一种数据库一个文件
#[cfg(sqlx_lock_pg)]
mod sqlx;

#[cfg(sqlx_lock_pg)]
pub(crate) use self::sqlx::PgLock;

/// 日志、错误、guard 里显示的 key
#[cfg(lock_pg)]
fn i64_name(key: i64) -> String {
    format!("advisory {key}")
}

#[cfg(lock_pg)]
fn pair_name(key1: i32, key2: i32) -> String {
    format!("advisory {key1},{key2}")
}

/// 哈希出来的锁显示原来的字符串 key，排查时能对上 `pg_locks` 里的数字
#[cfg(all(lock_pg, not(lock_row_pg)))]
fn hashed_name(hashed: i64, key: &hygiea_core::lock::LockKey) -> hygiea_core::Result<String> {
    Ok(format!("advisory {hashed} (hashed {})", key.as_str()?))
}

/// 给 PG 连接池实现 `DistributedLock`，连接池要有 `locker` 字段（各驱动的 PG 锁，现在只有 [`self::sqlx`] 的 `PgLock`；
/// 各驱动的锁方法名一样，共用这个宏）
#[cfg(lock_pg)]
macro_rules! impl_pg_lock {
    ($pool:ty) => {
        #[hygiea_core::app::async_trait]
        impl hygiea_core::lock::DistributedLock for $pool {
            async fn try_lock(
                &self,
                key: hygiea_core::lock::LockKey,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker.try_key(&key).await
            }

            async fn try_lock_i64(
                &self,
                key: impl Into<hygiea_core::lock::LockKeyI64> + Send,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker.try_i64(key.into().get()).await
            }

            async fn try_lock_i32(
                &self,
                key1: impl Into<hygiea_core::lock::LockType> + Send,
                key2: impl Into<hygiea_core::lock::LockKeyI32> + Send,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker
                    .try_pair(key1.into().key().get(), key2.into().get())
                    .await
            }

            async fn lock(
                &self,
                key: hygiea_core::lock::LockKey,
                timeout: std::time::Duration,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker.lock_key(&key, timeout).await
            }

            async fn lock_i64(
                &self,
                key: impl Into<hygiea_core::lock::LockKeyI64> + Send,
                timeout: std::time::Duration,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker.lock_i64(key.into().get(), timeout).await
            }

            async fn lock_i32(
                &self,
                key1: impl Into<hygiea_core::lock::LockType> + Send,
                key2: impl Into<hygiea_core::lock::LockKeyI32> + Send,
                timeout: std::time::Duration,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker
                    .lock_pair(key1.into().key().get(), key2.into().get(), timeout)
                    .await
            }
        }
    };
}
#[cfg(lock_pg)]
pub(crate) use impl_pg_lock;
