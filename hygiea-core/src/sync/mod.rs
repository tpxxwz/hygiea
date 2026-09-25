//! 并发访问控制：分布式锁抽象。

#[cfg(feature = "distributed-lock")]
mod lock;
#[cfg(feature = "distributed-lock")]
pub use lock::{DistributedKey, DistributedLock};
