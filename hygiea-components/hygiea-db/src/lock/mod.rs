//! 连接池的分布式锁（feature `lock`）：PG、MySQL 的连接池直接实现 hygiea-core 的 [`DistributedLock`](hygiea_core::lock::DistributedLock)，
//! 用法和 key 的规则见 `hygiea::lock`。
//!
//! | 后端 | `try_lock_i64` / `try_lock_i32` 和阻塞版 | 字符串 key（`try_lock` / `lock`） |
//! |---|---|---|
//! | PostgreSQL（`SqlxPgPool`、`SeaOrmPgPool`） | 原生事务级 advisory lock | 开了 `lock-row`：在 `{p}lock` 表（前缀默认是应用名加 `_`，见 [`PgLockConfig`]）里插入不提交的行，靠主键冲突互斥，精确；没开：返回 `LockErr::Unsupported`（见下） |
//! | MySQL（`SqlxMySqlPool`、`SeaOrmMySqlPool`） | 会话级命名锁 `GET_LOCK`，锁名 `i64:<数字>` / `i32:<类型>:<id>` | `GET_LOCK`，key 原样做锁名，精确，和 `lock-row` 无关（见下） |
//!
//! PG 每次加锁开一个事务（占一条连接），锁交给返回的 guard：`release()` 或 drop 时回滚事务，锁跟着释放；
//! 进程崩溃、连接断开时 PG 中止事务，锁也释放。持锁期间那条连接一直占着。
//!
//! 连接从哪来看 [`PgLockConfig`]（`[db.lock]` 段）：`connect`、`max_connections` 都没写时用连接池本身，同时持有的锁加上
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
//! # MySQL：`GET_LOCK`
//!
//! 会话级的命名锁，不开事务、不建表。锁在连接上，guard 一直占着这条连接，`release()` 或 drop 时 `RELEASE_LOCK`，
//! 进程崩溃、连接断开时 MySQL 释放。连接从哪来看 [`MySqlLockConfig`]，规则和 PG 一样。
//!
//! - try 是 `GET_LOCK(name, 0)`，立刻返回；阻塞版是原生等待，对方一释放马上拿到。`timeout` 必须是整数秒
//!   （`GET_LOCK` 按秒四舍五入，0.4 秒等于不等），至少 1 秒（0 除外），否则返回 `LockErr::InvalidTimeout`，不发 SQL
//! - 锁名最长 64 个字符：字符串 key 超过了返回 `LockErr::InvalidKey`，不截断、不哈希。整数 key 是 `i64:<数字>`、
//!   `i32:<类型>:<id>`，两个 key 空间，和字符串 key（以 9 位编号开头）也不会撞
//! - DBA 设的超时打断不了：等锁用 `SET @v = GET_LOCK(..)`（`max_execution_time` 只打断 SELECT）；拿锁的同一条语句把会话的
//!   `wait_timeout` 调到上限，持锁期间连接空闲也不会被断开，释放（或没拿到）时恢复成全局值再还回池。
//!   加锁、释放中途出错或被取消（连接上可能还挂着锁、`wait_timeout` 没恢复）时把连接关掉，不还回池
//! - 每把锁各占一条连接（一个会话），MySQL 的死锁检测管不到：调用方 A 持有 1 等 2、B 持有 2 等 1 时，
//!   两边等到各自的超时返回 `Ok(None)`（PG 每把锁一个事务，也一样）
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
//! # 没开 `lock-row` 时的字符串 key：不支持
//!
//! `try_lock` / `lock` 返回 [`LockErr::Unsupported`](hygiea_core::lock::LockErr::Unsupported)，不发任何 SQL、不建表，
//! 不会悄悄换成别的实现：字符串 key 只有锁表这一种语义。
//!
//! 想用字符串又不想建表的，调用方自己显式哈希：[`LockKeyI64::xxh3_64`](hygiea_core::lock::LockKeyI64::xxh3_64)`(&key)?`
//! 转成 i64 交给 `try_lock_i64`。不要自己换别的哈希（尤其是 `DefaultHasher`，每个进程种子不同，锁会失效）。
//! 这是调用方的取舍，要清楚：
//!
//! - 有损：不同 key 可能撞到同一个数，撞上的两个 key 互相多锁（不会少锁）。100 万个 key 里有一对碰撞的概率约 3 × 10⁻⁸
//! - 和直接传数字的 `try_lock_i64` 共用一个空间：哈希值正好等于别处传的数字时也是同一把锁
//! - 和锁表里同一个字符串 key 的那把锁是两样东西，互不互斥

use serde::Deserialize;

#[cfg(lock_mysql)]
use crate::config::MySqlConnectOverride;
#[cfg(lock_pg)]
use crate::config::PgConnectOverride;

/// 连接池配置里的 `lock` 段（PG 连接池，sqlx、SeaORM）。各项都不写时锁用连接池本身
#[cfg(lock_pg)]
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PgLockConfig {
    /// 锁表（`lock-row`）的表名前缀：不写用应用名（`[registry] app_name`）加 `_`，写 `""` 不加前缀（表名是 `lock`），
    /// 写 `foo` 或 `foo_` 都是 `foo_lock`。去掉末尾的 `_` 后得是小写下划线 `[a-z][a-z0-9_]*`。没开 `lock-row` 时不用
    pub table_prefix: Option<String>,
    /// 锁用的池最多几条连接（同时持有、等待的锁各占一条）。写了就单独建池，不写时沿用连接池的 `max_connections`
    pub max_connections: Option<u32>,
    /// 单独连的库：`[db.lock.connect]`，只写和连接池的 `[db.connect]` 不一样的项，不写的沿用。写了任意一项就单独建池
    pub connect: PgConnectOverride,
}

/// 连接池配置里的 `lock` 段（MySQL 连接池，sqlx、SeaORM）。各项都不写时锁用连接池本身
#[cfg(lock_mysql)]
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MySqlLockConfig {
    /// 锁用的池最多几条连接（同时持有、等待的锁各占一条）。写了就单独建池，不写时沿用连接池的 `max_connections`
    pub max_connections: Option<u32>,
    /// 单独连的库：`[db.lock.connect]`，只写和连接池的 `[db.connect]` 不一样的项，不写的沿用。写了任意一项就单独建池
    pub connect: MySqlConnectOverride,
}

// 规则（和数据库、驱动无关）。SQL 在 `sql/<数据库>/lock.rs`
#[cfg(lock_row_pg)]
mod table;
// 执行：每个驱动一个目录，里面一种数据库一个文件
#[cfg(any(sqlx_lock_pg, sqlx_lock_mysql))]
mod sqlx;

#[cfg(sqlx_lock_mysql)]
pub(crate) use self::sqlx::MySqlLock;
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

/// 给连接池实现 `DistributedLock`，连接池要有 `locker` 字段（各驱动、各数据库的锁，现在是 [`self::sqlx`] 的 `PgLock`、
/// `MySqlLock`；方法名都一样，共用这个宏）
#[cfg(lock_any)]
macro_rules! impl_lock {
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
#[cfg(lock_any)]
pub(crate) use impl_lock;
