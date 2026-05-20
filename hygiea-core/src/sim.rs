use std::collections::VecDeque;
use std::time::Duration;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use crate::date::{now_utc, set_now_utc};

static SIM_TIME: Mutex<Option<DateTime<Utc>>> = Mutex::new(None);
static SIM_QUEUE: Mutex<VecDeque<DateTime<Utc>>> = Mutex::new(VecDeque::new());

fn sim_now() -> DateTime<Utc> {
    SIM_TIME.lock().unwrap_or_else(Utc::now)
}

/// 仿真时钟，用于回测和测试场景。
///
/// 创建后 tokio 时间暂停，`tokio::time::sleep` 不消耗真实时间，
/// 只有通过 [`SimClock::advance_to`] 或 [`SimClock::advance_next`] 推进才会唤醒任务。
///
/// 生产代码里所有 `now_utc()` 和 `tokio::time::sleep` 照常写，无需改动。
pub struct SimClock;

impl SimClock {
    /// 初始化仿真时钟，从 `start` 时间点开始。
    /// 初始化仿真时钟。
    /// 调用前需确保 tokio 时间已暂停：测试中用 `#[tokio::test(start_paused = true)]`，
    /// 非测试环境在创建 runtime 时设置 `start_paused(true)`。
    pub fn new(start: DateTime<Utc>) -> Self {
        SIM_QUEUE.lock().clear();
        *SIM_TIME.lock() = Some(start);
        set_now_utc(sim_now);
        Self
    }

    /// 向队列中追加一个时间点。
    pub fn push(&self, t: DateTime<Utc>) {
        SIM_QUEUE.lock().push_back(t);
    }

    /// 从队列中取出下一个时间点并推进，队列为空时什么都不做。
    pub async fn advance_next(&self) {
        let next = match SIM_QUEUE.lock().pop_front() {
            Some(t) => t,
            None => return,
        };
        self.advance_to(next).await;
    }

    /// 推进到指定时间点，唤醒所有在此期间 sleep 到期的 tokio 任务。
    /// 若 `next` 不晚于当前时间则忽略。
    pub async fn advance_to(&self, next: DateTime<Utc>) {
        let prev = match *SIM_TIME.lock() {
            Some(t) => t,
            None => return,
        };
        if next <= prev {
            return;
        }
        let delta = (next - prev).to_std().unwrap_or(Duration::ZERO);
        *SIM_TIME.lock() = Some(next);
        // advance() fires expired timers (moves tasks to ready queue);
        // yield_now() lets those tasks actually run before we return.
        tokio::time::advance(delta).await;
        tokio::task::yield_now().await;
    }

}

impl Drop for SimClock {
    fn drop(&mut self) {
        *SIM_TIME.lock() = None;
        SIM_QUEUE.lock().clear();
        set_now_utc(Utc::now);
    }
}

// ========== TokenBucket ==========

/// 令牌桶限流器。
///
/// 在生产环境中使用真实时钟（`now_utc()`），在仿真/测试环境中
/// 配合 [`SimClock`] 使用——推进仿真时间即可驱动令牌补充，无需等待真实时间。
pub struct TokenBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: DateTime<Utc>,
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
        let elapsed = (now - self.last_refill).num_milliseconds().max(0) as f64 / 1000.0;
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
    use crate::date::now_utc;
    use serial_test::serial;
    use std::sync::Arc;
    use parking_lot::Mutex as PLMutex;
    use chrono::TimeZone;

    fn t(offset_secs: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()
            + chrono::Duration::seconds(offset_secs)
    }

    // ---- now_utc ------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_now_returns_start_time() {
        let _clock = SimClock::new(t(0));
        assert_eq!(now_utc(), t(0));
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_now_after_advance_to() {
        let clock = SimClock::new(t(0));
        clock.advance_to(t(10)).await;
        assert_eq!(now_utc(), t(10));
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_advance_to_earlier_time_ignored() {
        let clock = SimClock::new(t(100));
        clock.advance_to(t(50)).await;
        assert_eq!(now_utc(), t(100));
    }

    // ---- queue --------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_advance_next_through_queue() {
        let clock = SimClock::new(t(0));
        clock.push(t(10));
        clock.push(t(20));
        clock.push(t(30));

        clock.advance_next().await;
        assert_eq!(now_utc(), t(10));

        clock.advance_next().await;
        assert_eq!(now_utc(), t(20));

        clock.advance_next().await;
        assert_eq!(now_utc(), t(30));
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_advance_next_empty_queue_keeps_time() {
        let clock = SimClock::new(t(0));
        clock.advance_next().await;
        assert_eq!(now_utc(), t(0));
    }

    // ---- sleep --------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_sleep_wakes_on_advance() {
        let clock = SimClock::new(t(0));
        let woken = Arc::new(PLMutex::new(false));
        let woken_c = woken.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            *woken_c.lock() = true;
        });
        tokio::task::yield_now().await; // let spawned task poll once to register its timer

        clock.advance_to(t(4)).await;
        assert!(!*woken.lock(), "不应在 t+4s 醒来");

        clock.advance_to(t(5)).await;
        assert!(*woken.lock(), "应在 t+5s 醒来");
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn test_sleep_via_queue() {
        let clock = SimClock::new(t(0));
        clock.push(t(10));
        clock.push(t(20));

        let log = Arc::new(PLMutex::new(Vec::<i64>::new()));
        let log1 = log.clone();
        let log2 = log.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            log1.lock().push(10);
        });
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(20)).await;
            log2.lock().push(20);
        });
        tokio::task::yield_now().await; // let spawned tasks register their timers

        clock.advance_next().await; // → t(10)，task1 醒来
        assert_eq!(*log.lock(), vec![10]);

        clock.advance_next().await; // → t(20)，task2 醒来
        assert_eq!(*log.lock(), vec![10, 20]);
    }

    // ---- drop ---------------------------------------------------------------

    #[tokio::test]
    #[serial]
    async fn test_drop_restores_real_time() {
        {
            let _clock = SimClock::new(t(0));
            assert_eq!(now_utc(), t(0));
        }
        let real = now_utc();
        assert!(real.timestamp() > t(0).timestamp());
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
        let next = now_utc() + chrono::Duration::from_std(wait).expect("valid duration");
        clock.advance_to(next).await;
        assert_eq!(bucket.try_consume(), Duration::ZERO);
    }
}
