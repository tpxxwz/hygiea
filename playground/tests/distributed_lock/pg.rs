//! PostgreSQL 事务级 advisory lock，sqlx 和 SeaORM 两个连接池各一份。原来在 `hygiea-db` 的
//! `sqlx_postgres.rs`、`seaorm_postgres.rs`、`pg_advisory.rs`

use hygiea::app::async_trait;
use hygiea::db::{SeaOrmPgPool, SqlxPgPool};
use hygiea::{HyErr, ResultExt, err};

use super::core::{BaseLockErr, DistributedLock, LockGuard};

/// 字符串 key 转成 advisory lock 用的 i64：64 位 FNV-1a，跨进程、跨版本稳定。
/// 不同 key 撞上同一个哈希的概率可以忽略
pub(crate) fn advisory_key(key: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as i64
}

/// PostgreSQL 事务级 advisory lock：拿锁时开一个事务，在事务里 `pg_try_advisory_xact_lock`，
/// guard 持有这个事务，释放就是回滚。锁和事务绑在一起，进程崩溃、连接断开时数据库自动释放，
/// 不需要 TTL。
///
/// 持有锁期间占着一个连接，连接池要留出余量。字符串 key 用 64 位 FNV-1a 哈希成 advisory lock 的 i64，
/// 不同 key 撞上同一个哈希的概率可以忽略
#[async_trait]
impl DistributedLock for SqlxPgPool {
    async fn try_lock(&self, key: &str) -> Result<Option<LockGuard>, HyErr> {
        let acquire_failed = || err!(BaseLockErr::AcquireFailed, key);
        let mut tx = self.inner.begin().await.wrap_err(acquire_failed)?;
        let (acquired,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_xact_lock($1)")
            .bind(advisory_key(key))
            .fetch_one(&mut *tx)
            .await
            .wrap_err(acquire_failed)?;
        if !acquired {
            // 事务 drop 时回滚，连接还回池子
            return Ok(None);
        }
        let key_owned = key.to_string();
        Ok(Some(LockGuard::new(key, async move {
            tx.rollback()
                .await
                .wrap_err(|| err!(BaseLockErr::ReleaseFailed, key_owned))
        })))
    }
}

/// PostgreSQL 事务级 advisory lock：拿锁时开一个事务，在事务里 `pg_try_advisory_xact_lock`，
/// guard 持有这个事务，释放就是回滚。锁和事务绑在一起，进程崩溃、连接断开时数据库自动释放，
/// 不需要 TTL。
///
/// 持有锁期间占着一个连接，连接池要留出余量。字符串 key 用 64 位 FNV-1a 哈希成 advisory lock 的 i64，
/// 不同 key 撞上同一个哈希的概率可以忽略
#[async_trait]
impl DistributedLock for SeaOrmPgPool {
    async fn try_lock(&self, key: &str) -> Result<Option<LockGuard>, HyErr> {
        let acquire_failed = || err!(BaseLockErr::AcquireFailed, key);
        let mut tx = self
            .inner
            .get_postgres_connection_pool()
            .begin()
            .await
            .wrap_err(acquire_failed)?;
        let (acquired,): (bool,) = sea_orm::sqlx::query_as("SELECT pg_try_advisory_xact_lock($1)")
            .bind(advisory_key(key))
            .fetch_one(&mut *tx)
            .await
            .wrap_err(acquire_failed)?;
        if !acquired {
            // 事务 drop 时回滚，连接还回池子
            return Ok(None);
        }
        let key_owned = key.to_string();
        Ok(Some(LockGuard::new(key, async move {
            tx.rollback()
                .await
                .wrap_err(|| err!(BaseLockErr::ReleaseFailed, key_owned))
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::advisory_key;

    #[test]
    fn advisory_key_is_stable() {
        // FNV-1a 64 的标准测试向量，保证不同进程、不同版本算出来的一样
        assert_eq!(advisory_key(""), 0xcbf2_9ce4_8422_2325_u64 as i64);
        assert_eq!(advisory_key("a"), 0xaf63_dc4c_8601_ec8c_u64 as i64);
        assert_ne!(advisory_key("order:1"), advisory_key("order:2"));
    }
}
