//! 连接池的分布式锁（feature `lock`）：PG 的连接池直接实现 hygiea-core 的 [`DistributedLock`](hygiea_core::lock::DistributedLock)，
//! 用法和 key 的规则见 `hygiea::lock`。SQLite 还没有实现（持有写事务做锁会挡住整个库的写，
//! 租约行要有过期时间，方案没定）。
//!
//! | 后端 | `try_lock_i64` / `try_lock_i32` 和阻塞版 | 字符串 key（`try_lock` / `lock`） |
//! |---|---|---|
//! | PostgreSQL（`SqlxPgPool`、`SeaOrmPgPool`） | 原生事务级 advisory lock | 开了 `lock-row`：`{app_name}_lock` 表的行锁；没开返回 `LockErr::Unsupported` |
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

#[cfg(feature = "postgres")]
mod postgres;
#[cfg(all(feature = "postgres", feature = "lock-row"))]
mod table;

#[cfg(feature = "postgres")]
pub(crate) use postgres::{PgLock, impl_pg_lock};
