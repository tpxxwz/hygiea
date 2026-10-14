//! sqlx 专用的部分。feature `sqlx` 开了才有对外的：
//! - [`transaction`] 事务函数（`transaction.rs`）
//! - [`HyUtcDateTime`](crate::HyUtcDateTime) 的 trait 实现（`datetime.rs`）
//! - 连接池组件，一种数据库一个文件（`pool/`），再导出到 crate 根
//!
//! 另有不对外的 `internal/`：锁、动态配置的 sqlx 实现，sqlx、SeaORM 的连接池共用

#[cfg(feature = "sqlx")]
mod datetime;
#[cfg(any(sqlx_live_config, sqlx_lock_row))]
pub(crate) mod internal;
#[cfg(feature = "sqlx")]
pub(crate) mod pool;
#[cfg(feature = "sqlx")]
mod transaction;

#[cfg(feature = "sqlx")]
pub use transaction::transaction;
