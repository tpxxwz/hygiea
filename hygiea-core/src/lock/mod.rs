//! 分布式锁的抽象：[`DistributedLock`] trait、持有中的锁 [`LockGuard`]、key（[`LockKey`]、[`LockType`]、
//! [`LockKeyI32`] / [`LockKeyI64`]）、错误 [`LockErr`]。
//!
//! 后端（数据库、Redis）在各自的组件 crate 里实现，业务只依赖这里的 trait。不需要跨进程时可以用进程内的
//! `TokioLock`（feature `lock-tokio`）。
//!
//! 锁的 key 有三种，对标 PostgreSQL advisory lock。默认实现一层调一层：锁类型和 `i32` 合成一个 `i64`
//! 调 `*_i64`，`i64` 转成字符串 key 调字符串版本，后端最少只实现 [`try_lock`](DistributedLock::try_lock)；
//! 每个方法都可以覆盖（比如 PostgreSQL 直接用原生的整数 key）：
//!
//! | 方法 | key | 默认实现 | PostgreSQL 对应 |
//! |---|---|---|---|
//! | [`try_lock`](DistributedLock::try_lock) / [`lock`](DistributedLock::lock) | 字符串 [`LockKey`]（`lock_key!` 生成） | 后端实现 / 轮询 `try_lock` | 没有，后端自己映射 |
//! | [`try_lock_i64`](DistributedLock::try_lock_i64) / [`lock_i64`](DistributedLock::lock_i64) | 一个 `i64` | 转成字符串 key `999999999:<数字>` 调字符串版本 | `pg_try_advisory_xact_lock(bigint)` |
//! | [`try_lock_i32`](DistributedLock::try_lock_i32) / [`lock_i32`](DistributedLock::lock_i32) | 锁类型 [`LockType`] + 一个 `i32` | 高 32 位锁类型、低 32 位 `key2` 合成 `i64` 调 `*_i64` | `pg_try_advisory_xact_lock(int, int)` |
//!
//! 字符串 key 和锁类型都要注册（`#[derive(hy_lock_key)]` / `#[derive(hy_lock_type)]`），编号的组成和查重和错误码一样：
//! 项目前缀 3 位（和错误码共用 `err_code_project_prefix`）+ 模块前缀 + `code`，共 9 位，两个 derive 共用一套编号，全局唯一：
//! `#[lock_module_prefix = "02"]` 时 `code` 4 位，不写模块前缀时 6 位；项目前缀 `999` 和 `999999999` 留给 hygiea 自己。
//! 整数 key 的参数是 [`LockKeyI32`] / [`LockKeyI64`]，有符号、无符号的整数都能直接传：无符号的按位转，
//! 一一对应、不会撞，大的值在后端里看到的是负数。
//!
//! 锁不可重入：自己已经持有的 key 再加锁，和被别人占着一样拿不到。
//!
//! ```ignore
//! if let Some(guard) = lock.try_lock(lock_key!(JobLockKey::Cleanup)).await? {
//!     run_job().await?;
//!     guard.release().await?;
//! }
//! ```

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;

use crate::Result;

mod error;
mod guard;
mod key;
#[cfg(feature = "lock-tokio")]
mod tokio_lock;

pub use error::LockErr;
pub use guard::LockGuard;
/// 注册字符串 key 的模板，见 [`LockKey`]
pub use hygiea_macros::hy_lock_key;
/// 注册锁类型，见 [`LockType`]
pub use hygiea_macros::hy_lock_type;
pub use key::{LockKey, LockKeyI32, LockKeyI64, LockType};
#[cfg(feature = "lock-tokio")]
pub use tokio_lock::TokioLock;

pub(crate) use key::init;

#[doc(hidden)]
pub mod __private {
    pub use super::key::{LOCK_REGISTRATIONS, LockRegistration, lock_key, registered};
}

/// 生成字符串锁 key（[`LockKey`]），三种写法和 [`err!`](crate::err) 一样：
///
/// ```ignore
/// lock_key!(OrderLockKey::DailySettle)                          // 模板没有变量
/// lock_key!(OrderLockKey::Export, user_name)                    // 模板正好一个变量，直接给值
/// lock_key!(OrderLockKey::Sync, { "shop": shop_id, "day": day }) // 显式写出每个 key
/// ```
///
/// 写法和模板对不上、漏了模板变量，在编译期报错（规则同 `err!`）
#[macro_export]
macro_rules! lock_key {
    ($kind:expr $(, $($args:tt)*)?) => {
        $kind.__hygiea_lock_key($crate::__hygiea_tpl_args!(
            [
                "this lock key's template has variables, use lock_key!(X, value) or lock_key!(X, {{ .. }})",
                "this lock key's template has no variables, use lock_key!(X)",
                "lock_key!(X, value) needs a template with exactly one variable, use lock_key!(X) or lock_key!(X, {{ .. }})",
                "lock_key!: a template variable of this lock key is missing"
            ];
            $kind $(, $($args)*)?
        ))
    };
}
pub use crate::lock_key;

/// 阻塞版本默认实现里，两次尝试之间最多隔多久
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 分布式锁。后端必须实现 [`try_lock`](Self::try_lock)，其余方法的默认实现见[模块文档](self)，都可以覆盖。
///
/// 后端不想提供的方法照常实现，直接返回 [`LockErr::Unsupported`]：
///
/// ```ignore
/// async fn try_lock_i32(&self, _key1: impl Into<LockType> + Send, _key2: impl Into<LockKeyI32> + Send)
///     -> Result<Option<LockGuard>> {
///     Err(err!(LockErr::Unsupported, "redis: try_lock_i32"))
/// }
/// ```
///
/// 方法带泛型参数，不能做成 `dyn DistributedLock`，持有具体类型或者用泛型参数 `L: DistributedLock`
#[async_trait]
pub trait DistributedLock: Send + Sync {
    /// 字符串 key（[`LockKey`]）加锁，拿不到（被别人占着，包括被自己持有）立即返回 `Ok(None)`；
    /// 后端出错返回 [`LockErr::AcquireFailed`]。后端可以限制 key 的长度，超长返回 [`LockErr::InvalidKey`]，
    /// 不截断、不发给后端（见 [`LockKey`]）
    async fn try_lock(&self, key: LockKey) -> Result<Option<LockGuard>>;

    /// 一个 `i64` 做 key（[`LockKeyI64`]），其余同 [`try_lock`](Self::try_lock)。默认转成字符串 key `999999999:<数字>` 调 `try_lock`
    async fn try_lock_i64(&self, key: impl Into<LockKeyI64> + Send) -> Result<Option<LockGuard>> {
        self.try_lock(key::i64_key(key.into().get())).await
    }

    /// 锁类型 `key1`（[`LockType`]，注册过的）加一个 `i32` 的 `key2`（[`LockKeyI32`]）做 key，
    /// 其余同 [`try_lock`](Self::try_lock)。默认合成一个 `i64` 调 `try_lock_i64`
    async fn try_lock_i32(
        &self,
        key1: impl Into<LockType> + Send,
        key2: impl Into<LockKeyI32> + Send,
    ) -> Result<Option<LockGuard>> {
        self.try_lock_i64(LockKeyI64::from_pair(key1.into().key(), key2.into()))
            .await
    }

    /// 字符串 key 加锁，拿不到就等，最多等 `timeout`，超时返回 `Ok(None)`。`timeout` 为 0 等于 `try_lock`。
    /// 后端可以限制 `timeout` 的范围和精度（比如 PostgreSQL 是毫秒），超出范围返回 [`LockErr::InvalidTimeout`]，不发给后端。
    /// 默认每隔最多 100 毫秒反复 `try_lock`
    async fn lock(&self, key: LockKey, timeout: Duration) -> Result<Option<LockGuard>> {
        poll_until(timeout, || self.try_lock(key.clone())).await
    }

    /// 一个 `i64` 做 key，其余同 [`lock`](Self::lock)。默认转成字符串 key 调 `lock`
    async fn lock_i64(
        &self,
        key: impl Into<LockKeyI64> + Send,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        self.lock(key::i64_key(key.into().get()), timeout).await
    }

    /// 锁类型加一个 `i32` 做 key，其余同 [`lock`](Self::lock)。默认合成一个 `i64` 调 `lock_i64`
    async fn lock_i32(
        &self,
        key1: impl Into<LockType> + Send,
        key2: impl Into<LockKeyI32> + Send,
        timeout: Duration,
    ) -> Result<Option<LockGuard>> {
        self.lock_i64(
            LockKeyI64::from_pair(key1.into().key(), key2.into()),
            timeout,
        )
        .await
    }
}

/// 反复调 `attempt` 直到拿到锁、出错或超时；至少尝试一次
async fn poll_until<F, Fut>(timeout: Duration, mut attempt: F) -> Result<Option<LockGuard>>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<Option<LockGuard>>> + Send,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(guard) = attempt().await? {
            return Ok(Some(guard));
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(POLL_INTERVAL.min(deadline - now)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(hy_lock_type)]
    #[lock_internal_module_prefix = "091"]
    enum TestLockType {
        #[lock_type(code = "001")]
        A,
    }

    #[derive(hy_lock_key)]
    #[lock_internal_module_prefix = "091"]
    enum TestLockKey {
        #[lock_key(code = "002", tpl = "a")]
        A,
        #[lock_key(code = "003", tpl = "b")]
        B,
    }

    /// `TestLockType::A` 和 `key2` 合成的 i64 转成的字符串 key，也就是默认实现最后用的 key
    fn pair_key(key2: i32) -> String {
        let combined = (999_091_001i64 << 32) | (key2 as u32 as i64);
        format!("999999999:{combined}")
    }

    /// 进程内的假锁：记录尝试、释放了几次
    #[derive(Default)]
    struct FakeLock {
        held: Arc<Mutex<Vec<String>>>,
        released: Arc<AtomicUsize>,
        attempts: AtomicUsize,
    }

    /// 只实现 try_lock，其余走默认实现
    #[async_trait]
    impl DistributedLock for FakeLock {
        async fn try_lock(&self, key: LockKey) -> Result<Option<LockGuard>> {
            let key = key.as_str()?.to_string();
            self.attempts.fetch_add(1, Ordering::SeqCst);
            let mut held = self.held.lock().unwrap();
            if held.contains(&key) {
                return Ok(None);
            }
            held.push(key.clone());
            let (held, released) = (Arc::clone(&self.held), Arc::clone(&self.released));
            Ok(Some(LockGuard::new(key.clone(), async move {
                held.lock().unwrap().retain(|k| k != &key);
                released.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })))
        }
    }

    #[tokio::test]
    async fn release_frees_the_key() {
        let lock = FakeLock::default();
        let guard = lock
            .try_lock(lock_key!(TestLockKey::A))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(guard.key(), "999091002:a");
        // 不可重入：自己持有的也拿不到
        assert!(
            lock.try_lock(lock_key!(TestLockKey::A))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            lock.try_lock(lock_key!(TestLockKey::B))
                .await
                .unwrap()
                .is_some()
        );
        guard.release().await.unwrap();
        assert!(
            lock.try_lock(lock_key!(TestLockKey::A))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn dropped_guard_releases_in_background() {
        let lock = FakeLock::default();
        drop(lock.try_lock_i64(7).await.unwrap().unwrap());
        // 后台释放是 spawn 出去的，让它跑一下
        tokio::task::yield_now().await;
        assert_eq!(lock.released.load(Ordering::SeqCst), 1);
        assert!(lock.try_lock_i64(7).await.unwrap().is_some());
    }

    /// 整数 key 有符号、无符号都能直接传，无符号的按位转
    #[tokio::test]
    async fn integer_keys_accept_into() {
        let lock = FakeLock::default();
        let a = lock.try_lock_i64(7u32).await.unwrap().unwrap();
        assert_eq!(a.key(), "999999999:7");
        let big = lock.try_lock_i64(u64::MAX).await.unwrap().unwrap();
        assert_eq!(big.key(), "999999999:-1");
        let b = lock
            .try_lock_i32(TestLockType::A, 2u8)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(b.key(), pair_key(2));
        let c = lock
            .try_lock_i32(TestLockType::A, u32::MAX)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(c.key(), pair_key(-1));
    }

    /// 阻塞版本：拿不到就等，别人释放了就拿到
    #[tokio::test(start_paused = true)]
    async fn lock_waits_until_released() {
        let lock = Arc::new(FakeLock::default());
        let guard = lock
            .try_lock_i32(TestLockType::A, 2)
            .await
            .unwrap()
            .unwrap();
        let waiter = {
            let lock = Arc::clone(&lock);
            tokio::spawn(async move {
                lock.lock_i32(TestLockType::A, 2, Duration::from_secs(5))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(350)).await;
        guard.release().await.unwrap();
        let got = waiter.await.unwrap().unwrap();
        assert_eq!(got.map(|g| g.key().to_string()), Some(pair_key(2)));
    }

    /// 阻塞版本超时返回 None；timeout 为 0 只试一次
    #[tokio::test(start_paused = true)]
    async fn lock_times_out() {
        let lock = FakeLock::default();
        let _held = lock
            .try_lock(lock_key!(TestLockKey::A))
            .await
            .unwrap()
            .unwrap();
        let start = tokio::time::Instant::now();
        let got = lock
            .lock(lock_key!(TestLockKey::A), Duration::from_secs(1))
            .await;
        assert!(got.unwrap().is_none());
        assert_eq!(start.elapsed(), Duration::from_secs(1));

        let before = lock.attempts.load(Ordering::SeqCst);
        let got = lock.lock(lock_key!(TestLockKey::A), Duration::ZERO).await;
        assert!(got.unwrap().is_none());
        assert_eq!(lock.attempts.load(Ordering::SeqCst), before + 1);
    }

    /// 后端不提供的方法覆盖成返回 Unsupported，走默认实现的上层方法也跟着报
    #[tokio::test]
    async fn unsupported_method_returns_error() {
        struct NoI64;

        #[async_trait]
        impl DistributedLock for NoI64 {
            async fn try_lock(&self, _key: LockKey) -> Result<Option<LockGuard>> {
                Ok(None)
            }

            async fn try_lock_i64(
                &self,
                _key: impl Into<LockKeyI64> + Send,
            ) -> Result<Option<LockGuard>> {
                Err(crate::err!(LockErr::Unsupported, "test: try_lock_i64"))
            }
        }

        let e = NoI64.try_lock_i64(1).await.unwrap_err();
        assert!(e.is(LockErr::Unsupported));
        assert_eq!(
            e.to_string(),
            "Unsupported lock operation: test: try_lock_i64"
        );
        // try_lock_i32 的默认实现调 try_lock_i64，也拿到同一个错误
        let e = NoI64.try_lock_i32(TestLockType::A, 1).await.unwrap_err();
        assert!(e.is(LockErr::Unsupported));
    }

    /// 默认实现一层调一层：(锁类型, key2) 合成 i64，i64 转成字符串 key
    #[tokio::test]
    async fn defaults_delegate_down() {
        let lock = FakeLock::default();
        let combined = (999_091_001i64 << 32) | 42;
        let _held = lock.try_lock_i64(combined).await.unwrap().unwrap();
        assert!(
            lock.try_lock_i32(TestLockType::A, 42)
                .await
                .unwrap()
                .is_none()
        );
        let got = lock.lock_i32(TestLockType::A, 42, Duration::ZERO).await;
        assert!(got.unwrap().is_none());
    }
}
