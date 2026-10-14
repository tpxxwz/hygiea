//! 分布式锁的 MySQL SQL 和规则：会话级命名锁 `GET_LOCK`。锁在连接上，不开事务，`RELEASE_LOCK` 或连接断开时释放，
//! guard 要一直占着这条连接。整数 key 也是转成锁名用（MySQL 没有整数锁）。
//!
//! DBA 设的超时（实测 MySQL 8.4、9.7）：
//! - `max_execution_time` 只打断 SELECT：`SELECT GET_LOCK(..)` 等锁会被它打断，所以用 `SET @v = GET_LOCK(..)`，不受影响
//! - `wait_timeout`：持锁期间连接空闲超过它会被服务端断开，锁跟着释放。加锁的同一条语句把这个会话的值调到上限，
//!   释放（或没拿到）时恢复成全局值，连接再还回池
//! - `lock_wait_timeout`（元数据锁）、`innodb_lock_wait_timeout`（行锁）管不到 `GET_LOCK`

use std::time::Duration;

use hygiea_core::lock::LockErr;
use hygiea_core::{Result, err};

/// 加锁，参数是锁名、最多等多少秒（0 不等）。同一条语句把会话的 `wait_timeout` 调到上限（一年），结果放进会话变量，
/// 用 [`ACQUIRED`] 读
pub(crate) const ACQUIRE: &str =
    "SET SESSION wait_timeout = 31536000, @hygiea_lock = GET_LOCK(?, ?)";

/// [`ACQUIRE`] 的结果：拿到 1，等到超时 0，出错 NULL（比如等锁时连接被 KILL）
pub(crate) const ACQUIRED: &str = "SELECT @hygiea_lock";

/// 释放，并把会话的 `wait_timeout` 恢复成全局值，参数是锁名
pub(crate) const RELEASE: &str =
    "SET SESSION wait_timeout = DEFAULT, @hygiea_lock = RELEASE_LOCK(?)";

/// 没拿到锁：只恢复 `wait_timeout`
pub(crate) const RESTORE: &str = "SET SESSION wait_timeout = DEFAULT";

/// 锁名最长 64 个字符（超过 `GET_LOCK` 报 4163）
const MAX_NAME_CHARS: usize = 64;

/// 字符串 key 原样做锁名，超过 64 个字符报 [`LockErr::InvalidKey`]，不在发 SQL 之前悄悄截断或哈希
pub(crate) fn key_name(key: &str) -> Result<String> {
    let chars = key.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(err!(
            LockErr::InvalidKey,
            format!("mysql: lock name {key:?} is {chars} characters, max {MAX_NAME_CHARS}")
        ));
    }
    Ok(key.to_string())
}

/// `try_lock_i64` 的锁名。字符串 key 以 9 位数字开头，`i64:` 开头的不会和它们撞
pub(crate) fn i64_name(key: i64) -> String {
    format!("i64:{key}")
}

/// `try_lock_i32` 的锁名，和 [`i64_name`] 是两个 key 空间
pub(crate) fn pair_name(key1: i32, key2: i32) -> String {
    format!("i32:{key1}:{key2}")
}

/// 阻塞版的 `timeout` 转成 `GET_LOCK` 的秒数，加锁之前校验，不合法的不发给 MySQL：`GET_LOCK` 的超时按秒四舍五入
/// （0.4 秒等于不等、1.5 秒等于 2 秒），所以必须是整数秒、至少 1 秒，否则报错。调用方先把 0 分流到 `try_*`
pub(crate) fn timeout_secs(timeout: Duration) -> Result<u64> {
    if timeout.subsec_nanos() != 0 {
        return Err(err!(
            LockErr::InvalidTimeout,
            format!("mysql: timeout {timeout:?} is not a whole number of seconds")
        ));
    }
    match timeout.as_secs() {
        0 => Err(err!(
            LockErr::InvalidTimeout,
            format!("mysql: timeout {timeout:?} is less than 1s (0 means try_lock)")
        )),
        secs => Ok(secs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 个字符以内原样用（按字符数，不是字节数），超过报 InvalidKey
    #[test]
    fn long_names_are_rejected() {
        assert_eq!(key_name("999100002:job:1").unwrap(), "999100002:job:1");
        assert!(key_name(&"日".repeat(64)).is_ok());
        let e = key_name(&"x".repeat(65)).unwrap_err();
        assert!(e.is(LockErr::InvalidKey), "{e:#}");
        assert_eq!(i64_name(-1), "i64:-1");
        assert_eq!(pair_name(999100001, 42), "i32:999100001:42");
    }

    /// 整数秒转秒数；带零头的、0 报 InvalidTimeout
    #[test]
    fn timeout_rules() {
        assert_eq!(timeout_secs(Duration::from_secs(3)).unwrap(), 3);
        assert_eq!(
            timeout_secs(Duration::from_secs(u64::MAX)).unwrap(),
            u64::MAX
        );
        for bad in [
            Duration::from_millis(1500),
            Duration::from_millis(400),
            Duration::ZERO,
        ] {
            let e = timeout_secs(bad).unwrap_err();
            assert!(e.is(LockErr::InvalidTimeout), "{bad:?}: {e:#}");
        }
    }
}
