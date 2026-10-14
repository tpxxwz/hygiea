//! toasty 专用的部分，feature `toasty`：
//! - 连接池组件，一种数据库一个文件（`pool/`），再导出到 crate 根
//! - 不对外的 `internal/`：锁、动态配置的 toasty 实现共用的执行部分（PG）
//!
//! 没有事务函数、`HyUtcDateTime` 的实现。PG 的连接池有锁和动态配置（实现在 `lock/toasty/`、`live_config/toasty/`），
//! MySQL 的只有连接池

#[cfg(any(toasty_live_config, toasty_lock_pg))]
pub(crate) mod internal;
pub(crate) mod pool;
