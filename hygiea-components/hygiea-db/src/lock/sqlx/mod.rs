//! 锁的 sqlx 实现，一种数据库一个文件，这里只做声明和导出。规则在上层的 `lock/`，SQL 在 `sql/<数据库>/lock.rs`

#[cfg(sqlx_lock_mysql)]
mod mysql;
#[cfg(sqlx_lock_pg)]
mod postgres;

#[cfg(sqlx_lock_mysql)]
pub(crate) use mysql::MySqlLock;
#[cfg(sqlx_lock_pg)]
pub(crate) use postgres::PgLock;
