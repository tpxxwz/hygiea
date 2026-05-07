use std::future::Future;

use async_trait::async_trait;

// ---- key --------------------------------------------------------------------

pub enum DistributedKey {
    /// PostgreSQL advisory lock by a single i64 key.
    Advisory(i64),
    /// PostgreSQL advisory lock by two i32 keys.
    Advisory2(i32, i32),
    /// Named lock backed by a database row. The key length limit depends on
    /// the backend implementation (e.g. VARCHAR(255) for the PostgreSQL component).
    /// Excessively long keys will be rejected by the backend at runtime.
    Named(String),
}

impl From<i64> for DistributedKey {
    fn from(v: i64) -> Self {
        Self::Advisory(v)
    }
}

impl From<i32> for DistributedKey {
    fn from(v: i32) -> Self {
        Self::Advisory(v as i64)
    }
}

impl From<u32> for DistributedKey {
    fn from(v: u32) -> Self {
        Self::Advisory(v as i64)
    }
}

impl From<(i32, i32)> for DistributedKey {
    fn from((a, b): (i32, i32)) -> Self {
        Self::Advisory2(a, b)
    }
}

impl From<&str> for DistributedKey {
    fn from(v: &str) -> Self {
        debug_assert!(v.len() <= 255, "Named lock key exceeds 255 bytes: {}", v);
        Self::Named(v.to_string())
    }
}

impl From<String> for DistributedKey {
    fn from(v: String) -> Self {
        debug_assert!(v.len() <= 255, "Named lock key exceeds 255 bytes: {}", v);
        Self::Named(v)
    }
}

// ---- trait ------------------------------------------------------------------

#[async_trait]
pub trait DistributedLock {
    type Error;

    async fn lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<T, Self::Error>
    where
        K: Into<DistributedKey> + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<T, Self::Error>> + Send,
        T: Send + 'static;

    async fn try_lock<K, F, Fut, T>(&self, key: K, f: F) -> Result<Option<T>, Self::Error>
    where
        K: Into<DistributedKey> + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<T, Self::Error>> + Send,
        T: Send + 'static;
}
