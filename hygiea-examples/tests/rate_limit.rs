//! 令牌桶限流（暂存，不在 hygiea-core 里维护）。
//!
//! 原来放在 `hygiea-core/src/sync/rate_limit.rs`，以 `hygiea::sync::TokenBucket` 导出。core 暂时不考虑限流，
//! 所以挪到这里保留代码和测试；以后要在 core 里重新做限流，从这里开始。

use hygiea::datetime::now_utc;
use std::time::Duration;
use time::UtcDateTime;

// ========== TokenBucket ==========

/// 令牌桶限流器。
///
/// 按真实时钟（`now_utc()`）补充令牌。
pub struct TokenBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: UtcDateTime,
}

impl TokenBucket {
    /// 创建令牌桶。`max_requests` 为窗口内最大请求数，`window` 为补充周期。
    pub fn new(max_requests: u32, window: Duration) -> Self {
        let max = max_requests as f64;
        Self {
            tokens: max,
            max_tokens: max,
            refill_rate: max / window.as_secs_f64(),
            last_refill: now_utc(),
        }
    }

    fn refill(&mut self) {
        let now = now_utc();
        let elapsed = (now - self.last_refill).whole_milliseconds().max(0) as f64 / 1000.0;
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;
    }

    /// 消耗一个令牌。返回 `Duration::ZERO` 表示立即放行；
    /// 返回正值表示需要等待该时长后才有令牌可用。
    pub fn try_consume(&mut self) -> Duration {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / self.refill_rate)
        }
    }
}

// ---- token bucket (normal) ----------------------------------------------

#[test]
fn test_token_bucket_depletes() {
    // 3 tokens / 3 seconds = 1 token/sec
    let mut bucket = TokenBucket::new(3, Duration::from_secs(3));
    assert_eq!(bucket.try_consume(), Duration::ZERO);
    assert_eq!(bucket.try_consume(), Duration::ZERO);
    assert_eq!(bucket.try_consume(), Duration::ZERO);
    assert!(bucket.try_consume() > Duration::ZERO);
}

#[test]
fn test_token_bucket_wait_duration() {
    // 1 token / 10 seconds = 0.1 token/sec；耗尽后等待时间应为 10s
    let mut bucket = TokenBucket::new(1, Duration::from_secs(10));
    assert_eq!(bucket.try_consume(), Duration::ZERO);
    let wait = bucket.try_consume();
    assert!((wait.as_secs_f64() - 10.0).abs() < 0.01);
}
