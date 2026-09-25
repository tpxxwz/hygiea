//! 并发访问控制：本地限流、分布式锁抽象。

mod rate_limit;
pub use rate_limit::TokenBucket;

#[cfg(feature = "distributed-lock")]
mod lock;
#[cfg(feature = "distributed-lock")]
pub use lock::{DistributedKey, DistributedLock};
