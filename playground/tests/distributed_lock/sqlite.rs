//! SQLite 的进程内锁：只在同一个进程里互斥。原来在 `hygiea-db` 的 `sqlx_sqlite.rs`，实现在连接池上

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use hygiea::Result;
use hygiea::app::async_trait;
use tokio::sync::Mutex as AsyncMutex;

use super::core::{DistributedLock, LockGuard};

type LockRegistry = Mutex<HashMap<String, Weak<AsyncMutex<()>>>>;

static LOCKS: OnceLock<LockRegistry> = OnceLock::new();

/// 同一个数据库文件、同一个 key 共用一把锁；带上长度前缀，避免拼接后不同的组合撞成同一个字符串
fn lock_id(database: &str, key: &str) -> String {
    format!("{}:{database}:{}:{key}", database.len(), key.len())
}

fn keyed_lock(database: &str, key: &str) -> Arc<AsyncMutex<()>> {
    let id = lock_id(database, key);
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lock) = locks.get(&id).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(AsyncMutex::new(()));
    locks.insert(id, Arc::downgrade(&lock));
    lock
}

/// **进程内**的锁：只在同一个进程里互斥，不跨进程、不跨机器。
///
/// SQLite 是嵌入式数据库，一般一个进程独占一个数据库文件，进程内互斥就够了；
/// 多个进程共用一个文件、需要跨进程互斥时，换成 redis 或 PostgreSQL 的锁。
/// 进程退出锁自然就没了，不需要 TTL。
///
/// 按数据库区分锁：构造时传入数据库（一般就是 `SqlxSqliteConfig::database`），
/// 同一个数据库、同一个 key 共用一把锁
pub struct SqliteProcessLock {
    database: String,
}

impl SqliteProcessLock {
    pub fn new(database: impl Into<String>) -> Self {
        Self {
            database: database.into(),
        }
    }
}

#[async_trait]
impl DistributedLock for SqliteProcessLock {
    async fn try_lock(&self, key: &str) -> Result<Option<LockGuard>> {
        let lock = keyed_lock(&self.database, key);
        let Ok(held) = lock.try_lock_owned() else {
            return Ok(None);
        };
        Ok(Some(LockGuard::new(key, async move {
            drop(held);
            Ok(())
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn process_local_locks_are_keyed() {
        let lock = SqliteProcessLock::new(":memory:");

        let guard = lock.try_lock("same-key").await.unwrap().unwrap();
        // 同一个 key 拿不到，别的 key 不受影响
        assert!(lock.try_lock("same-key").await.unwrap().is_none());
        assert!(lock.try_lock("different-key").await.unwrap().is_some());
        // 同一个数据库的另一个实例共用锁，别的数据库不受影响
        assert!(
            SqliteProcessLock::new(":memory:")
                .try_lock("same-key")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            SqliteProcessLock::new("other.db")
                .try_lock("same-key")
                .await
                .unwrap()
                .is_some()
        );

        guard.release().await.unwrap();
        assert!(lock.try_lock("same-key").await.unwrap().is_some());
    }
}
