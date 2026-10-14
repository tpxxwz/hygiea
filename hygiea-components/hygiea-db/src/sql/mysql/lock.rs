//! 分布式锁的 MySQL SQL：用会话级的命名锁 `GET_LOCK`，锁在连接上，`RELEASE_LOCK` 或连接断开时释放
//! （和 PG 的事务级 advisory lock 不同：持锁时不开事务，guard 要占着这条连接）

/// 不等，参数是锁名：拿到 1，被别人持有 0，出错 NULL
pub(crate) const TRY: &str = "SELECT GET_LOCK(?, 0)";

/// 释放，参数是锁名：释放了 1，不是自己持有的 0，不存在 NULL
pub(crate) const RELEASE: &str = "SELECT RELEASE_LOCK(?)";

/// 锁名最长 64 个字符
const MAX_NAME: usize = 64;

/// 锁名：字符串 key 原样用，超过 64 个字符的哈希成 `hy:<数字>`（有损，可能和别的 key 撞上）
pub(crate) fn lock_name(key: &str) -> String {
    if key.chars().count() <= MAX_NAME {
        key.to_string()
    } else {
        format!("hy:{}", hygiea_core::string::xxh3_64(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_names_are_hashed() {
        assert_eq!(lock_name("999100002:job:1"), "999100002:job:1");
        let long = "x".repeat(65);
        assert!(lock_name(&long).starts_with("hy:"));
        assert!(lock_name(&long).len() <= MAX_NAME);
    }
}
