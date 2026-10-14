//! PostgreSQL 的 SQL 和规则，和驱动无关。各功能的在子模块（[`lock`]、[`live_config`]），这里放共用的。
//!
//! ## 事务的 BEGIN
//!
//! 框架自己在 PG 上开的事务（锁、建表）用 [`begin_sql`]：和 BEGIN 放在一条消息里（简单查询协议，一次往返）把会打断等待、
//! 断开持有的超时设成这个事务里的值，不受库、用户上 DBA 设的值影响，事务结束恢复原值，业务查询照旧受保护。
//! 经 PgBouncer transaction 模式也生效、不泄漏（`SET LOCAL` 只在事务里）。
//!
//! - `lock_timeout`：等锁最多多久，调用方给（0 是一直等）
//! - `statement_timeout`：0，不让整条语句的时限打断等锁
//! - `idle_in_transaction_session_timeout`、`transaction_timeout`：0，持锁期间业务在 Rust 里跑时事务是 idle in transaction，
//!   这两个非 0 时 PG 会断开连接，锁被悄悄释放
//! - 隔离级别固定 READ COMMITTED：语句结束就放掉快照，事务级 advisory lock 不挡同库的 vacuum（REPEATABLE READ 会留着 xmin），
//!   不跟着库的 `default_transaction_isolation` 走
//!
//! 中途某条 `SET LOCAL` 报错时服务端已经在事务里（失败状态），sqlx 不会回滚，连接直接还回池会让下一个使用者收到 25P02，
//! 调用方要回滚或关掉连接

#[cfg(live_config_pg)]
pub(crate) mod live_config;
#[cfg(lock_pg)]
pub(crate) mod lock;

/// 框架事务的 BEGIN，`lock_timeout_ms` 是毫秒（`lock_timeout` 不带单位就是毫秒），0 是一直等。
/// 值是框架自己的整数，拼进 SQL 安全
#[cfg(any(sqlx_lock_pg, all(sqlx_live_config, sqlx_pg)))]
pub(crate) fn begin_sql(lock_timeout_ms: i32) -> String {
    format!(
        "BEGIN ISOLATION LEVEL READ COMMITTED; \
         SET LOCAL lock_timeout = {lock_timeout_ms}; \
         SET LOCAL statement_timeout = 0; \
         SET LOCAL idle_in_transaction_session_timeout = 0; \
         SET LOCAL transaction_timeout = 0"
    )
}

/// 等锁超时（`lock_timeout` 到了）的 SQLSTATE：lock_not_available
#[cfg(lock_pg)]
pub(crate) const LOCK_NOT_AVAILABLE: &str = "55P03";

/// 建表时串行化：拿 `(LOCK_TYPE, Lock::*)` 这把事务级 advisory lock，参数是两个 int
#[cfg(any(live_config_pg, lock_row_pg))]
pub(crate) const DDL_LOCK: &str = "SELECT pg_advisory_xact_lock($1, $2)";

/// `current_schema()` 里这些表的列，参数是表名数组，每行 (表名, 列名, `udt_name`)，`udt_name` 是 `int8`、`bpchar` 这种。
/// 结果用 [`group_columns`](super::group_columns) 收成 [`Columns`](super::Columns)
#[cfg(any(live_config_pg, lock_row_pg))]
pub(crate) const COLUMNS: &str = "SELECT table_name::text, column_name::text, udt_name::text \
     FROM information_schema.columns \
     WHERE table_schema = current_schema() AND table_name = ANY($1) \
     ORDER BY table_name, ordinal_position";
