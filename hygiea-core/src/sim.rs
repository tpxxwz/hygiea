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

    // ---- tokio 虚拟时间 API 演示 ---------------------------------------------
    //
    // 以下测试仅演示 tokio::time 的虚拟时间 API（start_paused / advance /
    // sleep / timeout / interval / sleep_until），不涉及 SimClock 或 now_utc。

    /// 时间暂停后 sleep 永远不会自动完成，必须主动 advance 才会唤醒。
    #[tokio::test(start_paused = true)]
    async fn tokio_paused_time_never_wakes_sleep() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let woken = Arc::new(AtomicBool::new(false));
        let woken_c = woken.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            woken_c.store(true, Ordering::SeqCst);
        });
        // 让 spawned task 被调度，跑到 sleep.await 处挂起注册定时器；
        // 之后时间不推进，定时器永远不到期，woken 不会被设置。
        tokio::task::yield_now().await;
        assert!(!woken.load(Ordering::SeqCst));
    }

    /// tokio::time::advance 推进虚拟时间，到期 sleep 被唤醒。
    #[tokio::test(start_paused = true)]
    async fn tokio_advance_wakes_sleep() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let woken = Arc::new(AtomicBool::new(false));
        let woken_c = woken.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            woken_c.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert!(!woken.load(Ordering::SeqCst), "推进 2s 不足以唤醒 3s sleep");

        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(woken.load(Ordering::SeqCst), "累计 3s 后应被唤醒");
    }

    /// timeout 到期（inner > deadline）→ 返回 Err
    #[tokio::test(start_paused = true)]
    async fn tokio_timeout_expires() {
        let handle = tokio::spawn(async {
            tokio::time::timeout(
                Duration::from_secs(5),
                tokio::time::sleep(Duration::from_secs(10)),
            )
            .await
            .is_err()
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(handle.await.unwrap(), "5s deadline 已到，inner 10s 未完成");
    }

    /// timeout 在 deadline 之前完成 → 返回 Ok(value)
    #[tokio::test(start_paused = true)]
    async fn tokio_timeout_succeeds() {
        let handle = tokio::spawn(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                42u32
            })
            .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(handle.await.unwrap().unwrap(), 42);
    }

    /// tokio::time::interval 随虚拟时间逐步 tick；第一次 tick 立即返回。
    #[tokio::test(start_paused = true)]
    async fn tokio_interval_ticks() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        let count_c = count.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(10));
            loop {
                iv.tick().await;
                count_c.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "第一次 tick 立即触发");

        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    /// tokio::time::sleep_until 接受绝对 Instant。
    #[tokio::test(start_paused = true)]
    async fn tokio_sleep_until_absolute_instant() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let woken = Arc::new(AtomicBool::new(false));
        let woken_c = woken.clone();
        let wake_at = tokio::time::Instant::now() + Duration::from_secs(7);
        tokio::spawn(async move {
            tokio::time::sleep_until(wake_at).await;
            woken_c.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(6)).await;
        tokio::task::yield_now().await;
        assert!(!woken.load(Ordering::SeqCst));

        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(woken.load(Ordering::SeqCst));
    }

    /// tokio::time::timeout_at 接受绝对 Instant 截止时刻。
    #[tokio::test(start_paused = true)]
    async fn tokio_timeout_at_absolute_deadline() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let handle = tokio::spawn(async move {
            tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_secs(10)))
                .await
                .is_err()
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(handle.await.unwrap(), "deadline 已过，应超时");
    }

    /// tokio::time::interval_at 第一次 tick 不立即触发，而是等到指定 Instant。
    #[tokio::test(start_paused = true)]
    async fn tokio_interval_at_starts_at_specific_time() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        let count_c = count.clone();
        let start = tokio::time::Instant::now() + Duration::from_secs(5);
        tokio::spawn(async move {
            let mut iv = tokio::time::interval_at(start, Duration::from_secs(10));
            loop {
                iv.tick().await;
                count_c.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 0, "interval_at 不立即触发");

        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "t+5s 第一次 tick");

        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 2, "t+15s 第二次 tick");
    }

    /// pause() / resume() 显式控制虚拟/真实时钟切换。
    #[tokio::test(start_paused = true)]
    async fn tokio_pause_resume_explicit() {
        // 初始 paused，切回真实时钟后短 sleep 真的会等
        tokio::time::resume();
        let start = std::time::Instant::now();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(15),
            "resume 后 sleep 是真实时间, 实际 {:?}",
            elapsed
        );

        // 重新 pause，sleep 又得靠 advance 才能完成
        tokio::time::pause();
        let h = tokio::spawn(tokio::time::sleep(Duration::from_secs(1)));
        tokio::task::yield_now().await;
        assert!(!h.is_finished(), "重新 pause 后 sleep 不自完成");

        tokio::time::advance(Duration::from_secs(1)).await;
        h.await.unwrap(); // JoinHandle 同步比 yield+atomic 可靠
    }

    /// Sleep::reset 修改一个已创建 Sleep future 的目标时刻。
    #[tokio::test(start_paused = true)]
    async fn tokio_sleep_reset() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let woken = Arc::new(AtomicBool::new(false));
        let woken_c = woken.clone();
        tokio::spawn(async move {
            let sleep = tokio::time::sleep(Duration::from_secs(10));
            tokio::pin!(sleep);
            // 改主意：3s 后就醒
            sleep
                .as_mut()
                .reset(tokio::time::Instant::now() + Duration::from_secs(3));
            sleep.await;
            woken_c.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(3)).await;
        tokio::task::yield_now().await;
        assert!(woken.load(Ordering::SeqCst), "reset 后 3s 而非 10s 醒来");
    }

    /// Interval::reset_after 重新对齐下一次 tick 的时刻。
    #[tokio::test(start_paused = true)]
    async fn tokio_interval_reset_after() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        let count_c = count.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(10));
            iv.tick().await; // 第一次立即
            count_c.fetch_add(1, Ordering::SeqCst);
            // 本来下次在 t+10s，改成 3s 后
            iv.reset_after(Duration::from_secs(3));
            iv.tick().await;
            count_c.fetch_add(1, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_secs(3)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 2, "reset_after 让第二次 tick 提前到 3s");
    }

    /// MissedTickBehavior::Burst（默认）：advance 跨多个周期时，连续立即返回补齐错过的 tick。
    #[tokio::test(start_paused = true)]
    async fn tokio_interval_missed_burst() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        let count_c = count.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(10));
            // 默认就是 Burst，显式写出来更清楚
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
            loop {
                iv.tick().await;
                count_c.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "第一次 tick 立即");

        // 一口气跨 35s，期间错过 t=10, t=20, t=30 共 3 次
        tokio::time::advance(Duration::from_secs(35)).await;
        tokio::task::yield_now().await;
        // Burst 把 3 次错过的连续补齐
        assert_eq!(count.load(Ordering::SeqCst), 4, "Burst: 立即补齐 3 次");
    }

    /// MissedTickBehavior::Delay：错过 tick 只补一次，下一周期从"补这次的时刻"重新计。
    #[tokio::test(start_paused = true)]
    async fn tokio_interval_missed_delay() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        let count_c = count.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(10));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                iv.tick().await;
                count_c.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_secs(35)).await;
        tokio::task::yield_now().await;
        // Delay 只补一次，新 deadline = 此刻 + 10s = t=45
        assert_eq!(count.load(Ordering::SeqCst), 2, "Delay: 只补一次");
    }

    /// MissedTickBehavior::Skip：错过 tick 只补一次，下一次对齐到原周期网格上的下一个点。
    #[tokio::test(start_paused = true)]
    async fn tokio_interval_missed_skip() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = Arc::new(AtomicU32::new(0));
        let count_c = count.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(10));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                iv.tick().await;
                count_c.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_secs(35)).await;
        tokio::task::yield_now().await;
        // Skip 只补一次，下次对齐到 t=40（原周期网格上的下一个）
        assert_eq!(count.load(Ordering::SeqCst), 2, "Skip: 只补一次");
    }
}
