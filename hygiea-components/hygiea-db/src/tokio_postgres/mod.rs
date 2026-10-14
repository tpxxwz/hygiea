//! 直接用 tokio-postgres 的部分，feature `tokio-postgres`，和 sqlx 平级：
//! - 连接池组件（`pool.rs`，池是 deadpool-postgres），再导出到 crate 根
//! - 不对外的 `internal.rs`：锁、动态配置的 tokio-postgres 实现共用的执行部分
//!
//! 锁、动态配置的实现在 `lock/tokio_postgres/`、`live_config/tokio_postgres/`，SQL 和 sqlx 共用 `sql/postgres/`。
//! 现在只支持不加密的连接（NoTls），`sslmode=require` 连不上

#[cfg(any(tokio_pg_live_config, tokio_pg_lock))]
pub(crate) mod internal;
#[cfg(feature = "postgres")]
pub(crate) mod pool;
