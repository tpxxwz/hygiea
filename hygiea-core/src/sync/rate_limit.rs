//! 限流。

use crate::datetime::now_utc;
use std::time::Duration;
use time::UtcDateTime;

// ========== TokenBucket ==========

/// 令牌桶限流器。
///
/// 在生产环境中使用真实时钟（`now_utc()`），在仿真/测试环境中
/// 配合 [`crate::datetime::SimClock`] 使用——推进仿真时间即可驱动令牌补充，无需等待真实时间。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::SimClock;
    use serial_test::serial;
    use time::macros::date;

    fn t(offset_secs: i64) -> UtcDateTime {
        date!(2024 - 01 - 01).with_hms(0, 0, 0).unwrap().as_utc()
            + time::Duration::seconds(offset_secs)
    }

    // ---- token bucket (normal) ----------------------------------------------

    #[test]
    #[serial]
    fn test_token_bucket_depletes() {
        // 3 tokens / 3 seconds = 1 token/sec
        let mut bucket = TokenBucket::new(3, Duration::from_secs(3));
        assert_eq!(bucket.try_consume(), Duration::ZERO);
        assert_eq!(bucket.try_consume(), Duration::ZERO);
        assert_eq!(bucket.try_consume(), Duration::ZERO);
        assert!(bucket.try_consume() > Duration::ZERO);
    }

    #[test]
    #[serial]
    fn test_token_bucket_wait_duration() {
        // 1 token / 10 seconds = 0.1 token/sec；耗尽后等待时间应为 10s
        let mut bucket = TokenBucket::new(1, Duration::from_secs(10));
        assert_eq!(bucket.try_consume(), Duration::ZERO);
        let wait = bucket.try_consume();
        assert!((wait.as_secs_f64() - 10.0).abs() < 0.01);
    }

    // ---- token bucket (sim) -------------------------------------------------

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_token_bucket_refills_with_sim_time() {
        let clock = SimClock::new(t(0));
        // 3 tokens / 3 seconds = 1 token/sec
        let mut bucket = TokenBucket::new(3, Duration::from_secs(3));

        for _ in 0..3 {
            assert_eq!(bucket.try_consume(), Duration::ZERO);
        }
        assert!(bucket.try_consume() > Duration::ZERO);

        // advance 1s → 补充 1 个令牌
        clock.advance_to(t(1)).await;
        assert_eq!(bucket.try_consume(), Duration::ZERO);
        assert!(bucket.try_consume() > Duration::ZERO);

        // advance 3 more seconds → 补满 (max=3)
        clock.advance_to(t(4)).await;
        for _ in 0..3 {
            assert_eq!(bucket.try_consume(), Duration::ZERO);
        }
        assert!(bucket.try_consume() > Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_token_bucket_advance_by_wait_duration() {
        let clock = SimClock::new(t(0));
        // 1 token / 5 seconds = 0.2 token/sec
        let mut bucket = TokenBucket::new(1, Duration::from_secs(5));

        assert_eq!(bucket.try_consume(), Duration::ZERO);
        let wait = bucket.try_consume();
        assert_eq!(wait.as_secs(), 5);

        // 按照返回的等待时长推进仿真时间，重试应立即成功
        let next = now_utc() + time::Duration::try_from(wait).expect("valid duration");
        clock.advance_to(next).await;
        assert_eq!(bucket.try_consume(), Duration::ZERO);
    }
}
