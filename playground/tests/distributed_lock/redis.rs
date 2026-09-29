//! Redis 锁：`SET NX PX` + 随机 token、看门狗续期、Lua 比对 token 后释放。原来在 `hygiea-redis` 的 `fred_pool.rs`

use std::time::Duration;

use fred::prelude::*;
use hygiea::app::{CancellationToken, async_trait};
use hygiea::{HyErr, ResultExt, err};

use super::core::{BaseLockErr, DistributedLock, LockGuard};

/// 锁在 redis 里的 key：`hygiea:lock:<业务 key>`
const KEY_PREFIX: &str = "hygiea:lock:";

/// 值还是自己的 token 才删：锁已经过期、被别人拿走时不能删别人的锁
const RELEASE_SCRIPT: &str = "if redis.call('get', KEYS[1]) == ARGV[1] then \
     return redis.call('del', KEYS[1]) else return 0 end";

/// 值还是自己的 token 才续期
const RENEW_SCRIPT: &str = "if redis.call('get', KEYS[1]) == ARGV[1] then \
     return redis.call('pexpire', KEYS[1], ARGV[2]) else return 0 end";

/// 原来是给 `FredRedisPool` 实现的，过期时间取 `RedisConfig::lock_watchdog_timeout_secs`（默认 30 秒）。
/// 暂存时这个配置项已经从 `RedisConfig` 删掉，所以包成一个独立的类型，过期时间直接传
pub struct RedisLock {
    pub pool: Pool,
    pub ttl: Duration,
}

/// Redis 锁，做法同 Redisson：
///
/// - 加锁：`SET key token NX PX ttl`，token 是随机值，标识"这把锁是我的"
/// - 看门狗：持有期间每隔 ttl/3 续期一次（先比对 token），业务执行多久锁都不会中途过期；
///   进程崩溃续期就停了，最多 ttl 之后锁自动释放。ttl 是 [`RedisLock::ttl`]
/// - 释放：停掉看门狗，用 Lua 比对 token 再删，不会误删别人的锁
///
/// 注意 redis 主从复制是异步的，主节点刚写下锁就挂掉、
/// 从节点升主后这把锁会丢，别人能再拿一次；强一致的场景（比如资金）用 PostgreSQL 的锁
#[async_trait]
impl DistributedLock for RedisLock {
    async fn try_lock(&self, key: &str) -> Result<Option<LockGuard>, HyErr> {
        let redis_key = format!("{KEY_PREFIX}{key}");
        let ttl = self.ttl.max(Duration::from_secs(1));
        let token = random_token();
        let acquired: Option<String> = self
            .pool
            .set(
                &redis_key,
                token.as_str(),
                Some(Expiration::PX(ttl.as_millis() as i64)),
                Some(SetOptions::NX),
                false,
            )
            .await
            .wrap_err(|| err!(BaseLockErr::AcquireFailed, key))?;
        if acquired.is_none() {
            return Ok(None);
        }

        let stop = CancellationToken::new();
        tokio::spawn(watchdog(
            self.pool.clone(),
            redis_key.clone(),
            token.clone(),
            ttl,
            stop.clone(),
        ));

        let pool = self.pool.clone();
        let key_owned = key.to_string();
        Ok(Some(LockGuard::new(key, async move {
            stop.cancel();
            let released: i64 = pool
                .eval(RELEASE_SCRIPT, redis_key.as_str(), vec![token])
                .await
                .wrap_err(|| err!(BaseLockErr::ReleaseFailed, &key_owned))?;
            if released == 0 {
                tracing::warn!(
                    "lock {key_owned} was already expired or taken by others when released"
                );
            }
            Ok(())
        })))
    }
}

/// 每隔 ttl/3 续期一次，直到 guard 释放（`stop`）或者发现锁已经不是自己的。
/// 续期失败（redis 暂时连不上）只打 WARN 接着试，还有 2/3 的余量
async fn watchdog(pool: Pool, key: String, token: String, ttl: Duration, stop: CancellationToken) {
    let ttl_ms = ttl.as_millis().to_string();
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(ttl / 3) => {}
        }
        let renewed: Result<i64, _> = pool
            .eval(
                RENEW_SCRIPT,
                key.as_str(),
                vec![token.clone(), ttl_ms.clone()],
            )
            .await;
        match renewed {
            Ok(1) => {}
            Ok(_) => {
                tracing::warn!("lock {key} is no longer held (expired or taken), stop renewing");
                return;
            }
            Err(e) => tracing::warn!("renew lock {key} failed, will retry: {e}"),
        }
    }
}

/// 128 位随机 token。不引入随机数依赖：`RandomState` 每次创建的种子都是随机的
fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = || {
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    };
    format!("{:016x}{:016x}", part(), part())
}
