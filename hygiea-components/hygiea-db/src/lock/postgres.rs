//! PostgreSQL 的锁：整数 key 用事务级 advisory lock；字符串 key 开了 `lock-row` 在锁表里插入不提交的行，靠主键冲突互斥，
//! 没开就哈希成 i64（`LockKey::hashed_i64`）走 advisory lock，有损，见 lock 模块文档

use std::time::Duration;

use sqlx::{PgPool, Postgres, Transaction};

use hygiea_core::app::RegistryConfig;
use hygiea_core::lock::{LockErr, LockGuard, LockKey};
use hygiea_core::{Result, ResultExt, err};

#[cfg(feature = "lock-row")]
use super::table::LockTable;

/// 连接池上锁的实现，`SqlxPgPool`、`SeaOrmPgPool` 各持有一个（主池的 sqlx 池），对外的方法见 [`impl_pg_lock`]
#[derive(Clone, Debug)]
pub(crate) struct PgLock {
    pool: PgPool,
    #[cfg(feature = "lock-row")]
    table: LockTable,
}

impl PgLock {
    /// 只建对象，不连库；锁表在 [`prepare`](Self::prepare) 里建
    pub(crate) fn new(pool: PgPool, registry_config: &RegistryConfig) -> Result<Self> {
        #[cfg(not(feature = "lock-row"))]
        let _ = registry_config;
        Ok(Self {
            pool,
            #[cfg(feature = "lock-row")]
            table: LockTable::new(&registry_config.app_name)?,
        })
    }

    /// 启动时调用：开了 `lock-row` 就检查、建锁表
    pub(crate) async fn prepare(&self) -> Result<()> {
        #[cfg(feature = "lock-row")]
        self.table.ensure(&self.pool).await?;
        Ok(())
    }

    pub(crate) async fn try_i64(&self, key: i64) -> Result<Option<LockGuard>> {
        self.try_advisory(key, format!("advisory {key}")).await
    }

    /// `name` 是日志、错误、guard 里显示的 key
    async fn try_advisory(&self, key: i64, name: String) -> Result<Option<LockGuard>> {
        let mut tx = self.begin(&name).await?;
        let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(key)
            .fetch_one(&mut *tx)
            .await
            .wrap_err(|| acquire_err(&name))?;
        Ok(guard(tx, name, got))
    }

    pub(crate) async fn try_pair(&self, key1: i32, key2: i32) -> Result<Option<LockGuard>> {
        let name = format!("advisory {key1},{key2}");
        let mut tx = self.begin(&name).await?;
        let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1, $2)")
            .bind(key1)
            .bind(key2)
            .fetch_one(&mut *tx)
            .await
            .wrap_err(|| acquire_err(&name))?;
        Ok(guard(tx, name, got))
    }

    pub(crate) async fn lock_i64(&self, key: i64, timeout: Duration) -> Result<Option<LockGuard>> {
        self.lock_advisory(key, format!("advisory {key}"), timeout)
            .await
    }

    async fn lock_advisory(
        &self,
        key: i64,
        name: String,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        if timeout.is_zero() {
            return self.try_advisory(key, name).await;
        }
        let millis = timeout_millis(timeout)?;
        let mut tx = self.begin(&name).await?;
        set_lock_timeout(&mut tx, millis, &name).await?;
        let result = sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(key)
            .execute(&mut *tx)
            .await;
        let got = waited(result.map(|_| ()), &name)?;
        Ok(guard(tx, name, got))
    }

    pub(crate) async fn lock_pair(
        &self,
        key1: i32,
        key2: i32,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        if timeout.is_zero() {
            return self.try_pair(key1, key2).await;
        }
        let millis = timeout_millis(timeout)?;
        let name = format!("advisory {key1},{key2}");
        let mut tx = self.begin(&name).await?;
        set_lock_timeout(&mut tx, millis, &name).await?;
        let result = sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(key1)
            .bind(key2)
            .execute(&mut *tx)
            .await;
        let got = waited(result.map(|_| ()), &name)?;
        Ok(guard(tx, name, got))
    }

    /// 字符串 key：在锁表里插入 key 那一行、不提交。INSERT 没有 `NOWAIT` / `SKIP LOCKED`，
    /// 别人持有时最多等 1 毫秒（`lock_timeout` 的最小值，0 在 PG 里是不限时），等不到返回 `None`
    #[cfg(feature = "lock-row")]
    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        self.insert_key(key, 1).await
    }

    #[cfg(feature = "lock-row")]
    pub(crate) async fn lock_key(
        &self,
        key: &LockKey,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        if timeout.is_zero() {
            return self.try_key(key).await;
        }
        self.insert_key(key, timeout_millis(timeout)?).await
    }

    /// 插入 key 那一行，最多等 `millis` 毫秒。撞上已经提交的行（被人手动插入）报 23505，作为加锁失败报出来
    #[cfg(feature = "lock-row")]
    async fn insert_key(&self, key: &LockKey, millis: i32) -> Result<Option<LockGuard>> {
        let name = key.as_str()?.to_string();
        self.table.ensure(&self.pool).await?;
        let mut tx = self.begin(&name).await?;
        set_lock_timeout(&mut tx, millis, &name).await?;
        let result = sqlx::query(sqlx::AssertSqlSafe(self.table.insert()))
            .bind(&name)
            .execute(&mut *tx)
            .await;
        let got = waited(result.map(|_| ()), &name)?;
        Ok(guard(tx, name, got))
    }

    /// 没开 `lock-row`：字符串 key 哈希成 i64 走 advisory lock，不同 key 可能撞到同一把锁
    #[cfg(not(feature = "lock-row"))]
    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        let hashed = key.hashed_i64()?.get();
        self.try_advisory(hashed, hashed_name(hashed, key)?).await
    }

    #[cfg(not(feature = "lock-row"))]
    pub(crate) async fn lock_key(
        &self,
        key: &LockKey,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        let hashed = key.hashed_i64()?.get();
        self.lock_advisory(hashed, hashed_name(hashed, key)?, timeout)
            .await
    }

    async fn begin(&self, name: &str) -> Result<Transaction<'static, Postgres>> {
        self.pool.begin().await.wrap_err(|| acquire_err(name))
    }
}

/// 哈希出来的锁显示原来的字符串 key，排查时能对上 `pg_locks` 里的数字
#[cfg(not(feature = "lock-row"))]
fn hashed_name(hashed: i64, key: &LockKey) -> Result<String> {
    Ok(format!("advisory {hashed} (hashed {})", key.as_str()?))
}

fn acquire_err(name: &str) -> hygiea_core::HyErr {
    err!(LockErr::AcquireFailed, name)
}

/// 拿到了：事务交给 guard，释放时回滚；没拿到：事务直接丢掉（sqlx 归还连接时回滚）
fn guard(tx: Transaction<'static, Postgres>, name: String, got: bool) -> Option<LockGuard> {
    if !got {
        return None;
    }
    let key = name.clone();
    Some(LockGuard::new(name, async move {
        tx.rollback()
            .await
            .wrap_err(|| err!(LockErr::ReleaseFailed, key))
    }))
}

/// 阻塞版的 `timeout` 转成 `lock_timeout` 的毫秒数，在开事务之前校验，不合法的不发给 PG：
/// 必须是整数毫秒（`lock_timeout` 的单位是毫秒），在 1 毫秒到 `i32::MAX` 毫秒（约 24.8 天，`lock_timeout` 的上限）之间，
/// 否则报错。调用方先把 0 分流到 `try_*`
fn timeout_millis(timeout: Duration) -> Result<i32> {
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

/// 这个事务里等锁最多等多久（`SET LOCAL lock_timeout`），`millis` 由 [`timeout_millis`] 校验过
async fn set_lock_timeout(
    tx: &mut Transaction<'static, Postgres>,
    millis: i32,
    name: &str,
) -> Result<()> {
    sqlx::query("SELECT set_config('lock_timeout', $1, true)")
        .bind(format!("{millis}ms"))
        .execute(&mut **tx)
        .await
        .wrap_err(|| acquire_err(name))?;
    Ok(())
}

/// 阻塞等锁的结果：拿到是 `true`，超时（55P03 lock_not_available）是 `false`，其他错误照常报
fn waited(result: sqlx::Result<()>, name: &str) -> Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(e) if is_lock_timeout(&e) => Ok(false),
        Err(e) => Err(e).wrap_err(|| acquire_err(name)),
    }
}

fn is_lock_timeout(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|code| code == "55P03")
}

/// 给连接池实现 `DistributedLock`，连接池要有 `locker: PgLock` 字段
macro_rules! impl_pg_lock {
    ($pool:ty) => {
        #[hygiea_core::app::async_trait]
        impl hygiea_core::lock::DistributedLock for $pool {
            async fn try_lock(
                &self,
                key: hygiea_core::lock::LockKey,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker.try_key(&key).await
            }

            async fn try_lock_i64(
                &self,
                key: impl Into<hygiea_core::lock::LockKeyI64> + Send,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker.try_i64(key.into().get()).await
            }

            async fn try_lock_i32(
                &self,
                key1: impl Into<hygiea_core::lock::LockType> + Send,
                key2: impl Into<hygiea_core::lock::LockKeyI32> + Send,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker
                    .try_pair(key1.into().key().get(), key2.into().get())
                    .await
            }

            async fn lock(
                &self,
                key: hygiea_core::lock::LockKey,
                timeout: std::time::Duration,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker.lock_key(&key, timeout).await
            }

            async fn lock_i64(
                &self,
                key: impl Into<hygiea_core::lock::LockKeyI64> + Send,
                timeout: std::time::Duration,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker.lock_i64(key.into().get(), timeout).await
            }

            async fn lock_i32(
                &self,
                key1: impl Into<hygiea_core::lock::LockType> + Send,
                key2: impl Into<hygiea_core::lock::LockKeyI32> + Send,
                timeout: std::time::Duration,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker
                    .lock_pair(key1.into().key().get(), key2.into().get(), timeout)
                    .await
            }
        }
    };
}
pub(crate) use impl_pg_lock;

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
