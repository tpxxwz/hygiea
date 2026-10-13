//! 连接池的分布式锁（feature `lock`）：PG 的连接池直接实现 hygiea-core 的 [`DistributedLock`](hygiea_core::lock::DistributedLock)，
//! 用法和 key 的规则见 `hygiea::lock`。
//!
//! | 后端 | `try_lock_i64` / `try_lock_i32` 和阻塞版 | 字符串 key（`try_lock` / `lock`） |
//! |---|---|---|
//! | PostgreSQL（`SqlxPgPool`、`SeaOrmPgPool`） | 原生事务级 advisory lock | 开了 `lock-row`：`{app_name}_lock` 表的行锁，精确；没开：哈希成 i64 走 advisory lock，有损（见下） |
//!
//! PG 每次加锁从主池开一个事务（占一条连接），锁交给返回的 guard：`release()` 或 drop 时回滚事务，锁跟着释放；
//! 进程崩溃、连接断开时 PG 中止事务，锁也释放。持锁期间那条连接一直占着，同时持有的锁加上业务查询要在
//! `max_connections` 以内。
//!
//! - `try_lock_i32` 用原生的 `(int, int)` 形式：PG 上它和 `try_lock_i64` 是两个 key 空间（`(0, 42)` 和 `42` 是两把锁），
//!   和 hygiea-core 的默认实现（合成一个 i64）不同
//! - 阻塞版用 `lock_timeout` 原生等待，超时返回 `Ok(None)`；`timeout` 为 0 等于 `try_*`
//! - 锁表的建表规则和动态配置一样：表在且列对上就用，不在就建，列对不上拒绝启动。启动时建（`connect_lazy` 时第一次用字符串 key 时建）。
//!   表里一个 key 一行，加锁时补上，不删
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

#[cfg(feature = "postgres")]
mod postgres;
#[cfg(all(feature = "postgres", feature = "lock-row"))]
mod table;

#[cfg(feature = "postgres")]
pub(crate) use postgres::{PgLock, impl_pg_lock};
