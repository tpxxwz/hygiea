//! 分布式锁（暂存，不在 hygiea 里维护）。
//!
//! 原来是 `hygiea::sync` 的 `DistributedLock` 抽象，加上各连接池的实现（redis：`SET NX` + 看门狗；
//! PostgreSQL：事务级 advisory lock；SQLite：进程内锁）。移出的原因：分布式锁要做对很难，阻塞等待要么占连接、
//! 要么轮询、要么依赖通知，租约方案还有过期后两个人同时持有的问题；而且锁和业务 SQL 不在同一个连接、
//! 同一个事务里，谈不上事务一致性。需要互斥时直接在业务里用数据库：`INSERT ... ON CONFLICT DO NOTHING`、
//! `SELECT ... FOR UPDATE`，或者在业务事务里 `pg_try_advisory_xact_lock`。
//!
//! 以后要重新做，从这里开始。各模块：
//!
//! | 模块 | 内容 |
//! |---|---|
//! | `core` | `BaseLockErr`、`LockGuard`、`DistributedLock` |
//! | `poll` | 轮询等待（原来的 `DistributedLock::lock` 默认实现） |
//! | `redis` | Redis 锁（包成 `RedisLock { pool, ttl }`，原来给 `FredRedisPool` 实现） |
//! | `pg` | sqlx / SeaORM 的 PostgreSQL advisory lock |
//! | `sqlite` | SQLite 进程内锁 |
//! | `redis_it`、`pg_it` | 需要真实服务的端到端测试，默认 ignore |

mod core;
mod pg;
mod pg_it;
mod poll;
mod redis;
mod redis_it;
mod sqlite;
