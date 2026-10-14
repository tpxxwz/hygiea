//! MySQL 锁的 sqlx 实现：`GET_LOCK` 会话级命名锁（SQL 在 `sql/mysql/lock.rs`）。只实现了 `try_lock`，
//! 整数 key 和阻塞版用 `DistributedLock` 的默认实现（整数转成字符串 key，阻塞版轮询）

use sqlx::MySqlPool;
use sqlx::pool::PoolConnection;
use sqlx::{MySql, MySqlConnection};

use hygiea_core::lock::{LockErr, LockGuard, LockKey};
use hygiea_core::{Result, ResultExt, err};

use crate::sql::mysql::lock::{RELEASE, TRY, lock_name};

/// 连接池上锁的实现，`SqlxMySqlPool` 持有，对外的方法见 [`impl_mysql_lock`]
#[derive(Clone, Debug)]
pub(crate) struct MySqlLock {
    pool: MySqlPool,
}

impl MySqlLock {
    pub(crate) fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn try_key(&self, key: &LockKey) -> Result<Option<LockGuard>> {
        let key = key.as_str()?.to_string();
        let name = lock_name(&key);
        let acquire_err = || err!(LockErr::AcquireFailed, key.clone());
        let mut held = Held {
            conn: self.pool.acquire().await.wrap_err(acquire_err)?,
            name: name.clone(),
            locked: false,
        };
        let got: Option<i64> = sqlx::query_scalar(TRY)
            .bind(name)
            .fetch_one(held.conn())
            .await
            .wrap_err(acquire_err)?;
        if got != Some(1) {
            // 没拿到：连接上没有锁，正常还回池
            return Ok(None);
        }
        held.locked = true;
        Ok(Some(LockGuard::new(key.clone(), async move {
            held.release()
                .await
                .wrap_err(|| err!(LockErr::ReleaseFailed, key))
        })))
    }
}

/// 持锁的连接。锁在会话上，没走到 [`release`](Self::release) 就被丢掉（出错、取消）时把连接关掉，断开即释放，
/// 不能带着锁还回池
struct Held {
    conn: PoolConnection<MySql>,
    name: String,
    locked: bool,
}

impl Held {
    fn conn(&mut self) -> &mut MySqlConnection {
        &mut self.conn
    }

    async fn release(mut self) -> sqlx::Result<()> {
        let name = std::mem::take(&mut self.name);
        let _: Option<i64> = sqlx::query_scalar(RELEASE)
            .bind(name)
            .fetch_one(self.conn())
            .await?;
        self.locked = false;
        Ok(())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.locked {
            self.conn.close_on_drop();
        }
    }
}

/// 给连接池实现 `DistributedLock`，连接池要有 `locker: MySqlLock` 字段
macro_rules! impl_mysql_lock {
    ($pool:ty) => {
        #[hygiea_core::app::async_trait]
        impl hygiea_core::lock::DistributedLock for $pool {
            async fn try_lock(
                &self,
                key: hygiea_core::lock::LockKey,
            ) -> hygiea_core::Result<Option<hygiea_core::lock::LockGuard>> {
                self.locker.try_key(&key).await
            }
        }
    };
}
pub(crate) use impl_mysql_lock;
