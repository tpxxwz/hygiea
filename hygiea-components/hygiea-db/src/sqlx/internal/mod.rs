//! 锁、动态配置的 sqlx 实现共用的执行部分（SQL 文本和规则在 [`crate::sql`]）。sqlx、SeaORM 的连接池用这一份：
//! SeaORM 底下就是 sqlx，交出它的 sqlx 池即可。别的驱动要有锁、动态配置，在它自己的框架目录下
//! 另写执行部分，SQL 文本和规则复用 [`crate::sql`]。
//!
//! 不对外：只开 SeaORM 时 `sqlx` 模块也会因为这里而编译，但里面没有公开的东西

#[cfg(all(sqlx_live_config, sqlx_mysql))]
pub(crate) mod mysql;
#[cfg(all(sqlx_pg, any(sqlx_live_config, sqlx_lock_row)))]
pub(crate) mod postgres;
#[cfg(all(sqlx_live_config, sqlx_sqlite))]
pub(crate) mod sqlite;
