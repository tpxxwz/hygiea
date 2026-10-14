//! 分布式锁的 PostgreSQL SQL 和规则：整数 key 用事务级 advisory lock，字符串 key（`lock-row`）在锁表里插入不提交的行。
//! 每把锁的事务用 [`super::begin_sql`] 开，`lock_timeout` 按方法设

use std::time::Duration;

use hygiea_core::lock::LockErr;
use hygiea_core::{Result, ResultExt, err};

// ---- advisory lock -------------------------------------------------------------

/// 不等，拿到返回 true。参数是 i64
pub(crate) const TRY_I64: &str = "SELECT pg_try_advisory_xact_lock($1)";
/// 不等，拿到返回 true。参数是两个 int（锁类型、key）
pub(crate) const TRY_PAIR: &str = "SELECT pg_try_advisory_xact_lock($1, $2)";
/// 等到拿到，或者事务的 `lock_timeout` 到了报 [`LOCK_NOT_AVAILABLE`](super::LOCK_NOT_AVAILABLE)
pub(crate) const LOCK_I64: &str = "SELECT pg_advisory_xact_lock($1)";
pub(crate) const LOCK_PAIR: &str = "SELECT pg_advisory_xact_lock($1, $2)";

/// 阻塞版的 `timeout` 转成 `lock_timeout` 的毫秒数，在开事务之前校验，不合法的不发给 PG：
/// 必须是整数毫秒（`lock_timeout` 的单位是毫秒），在 1 毫秒到 `i32::MAX` 毫秒（约 24.8 天，`lock_timeout` 的上限）之间，
/// 否则报错。调用方先把 0 分流到 `try_*`
pub(crate) fn timeout_millis(timeout: Duration) -> Result<i32> {
    if !timeout.subsec_nanos().is_multiple_of(1_000_000) {
        return Err(err!(
            LockErr::InvalidTimeout,
            format!("postgres: timeout {timeout:?} is not a whole number of milliseconds")
        ));
    }
    let millis = timeout.as_millis();
    if millis == 0 {
        return Err(err!(
            LockErr::InvalidTimeout,
            format!("postgres: timeout {timeout:?} is less than 1ms (0 means try_lock)")
        ));
    }
    i32::try_from(millis).wrap_err(|| {
        err!(
            LockErr::InvalidTimeout,
            format!("postgres: timeout {timeout:?} exceeds {}ms", i32::MAX)
        )
    })
}

// ---- 锁表（lock-row） -------------------------------------------------------------

/// 建表语句：`{p}` 换成前缀（应用名加 `_`）
#[cfg(lock_row_pg)]
pub(crate) const TABLE_DDL: &str = include_str!("lock.sql");

/// 锁表期望的列：(列名, udt_name)。改了 `lock.sql` 的列要同时改这里
#[cfg(lock_row_pg)]
pub(crate) const TABLE_COLUMNS: [(&str, &str); 1] = [("key", "text")];

/// 表的持久化方式，参数是表名，`p` 普通表、`u` UNLOGGED，表不在时没有行
#[cfg(lock_row_pg)]
pub(crate) const TABLE_PERSISTENCE: &str = "SELECT c.relpersistence::text FROM pg_class c \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = current_schema() AND c.relname = $1";

/// 加锁：插入 key 那一行，不提交。别人正持有（插了没提交）时等它的事务结束，最多等事务的 `lock_timeout`
#[cfg(lock_row_pg)]
pub(crate) fn table_insert(table: &str) -> String {
    format!("INSERT INTO {table} (key) VALUES ($1)")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 毫秒到 i32::MAX 毫秒之间的整数毫秒转成毫秒数；带零头的、超出范围的报 InvalidTimeout
    #[test]
    fn timeout_millis_range() {
        assert_eq!(timeout_millis(Duration::from_millis(1)).unwrap(), 1);
        assert_eq!(timeout_millis(Duration::from_secs(3)).unwrap(), 3000);
        assert_eq!(
            timeout_millis(Duration::from_millis(i32::MAX as u64)).unwrap(),
            i32::MAX
        );
        for bad in [
            Duration::from_micros(999),
            Duration::from_micros(1_900),
            Duration::from_nanos(1_000_001),
            Duration::from_millis(i32::MAX as u64 + 1),
            Duration::MAX,
        ] {
            let e = timeout_millis(bad).unwrap_err();
            assert!(e.is(LockErr::InvalidTimeout), "{bad:?}: {e:#}");
        }
    }
}
