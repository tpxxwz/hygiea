//! 按配置得出取消信号：配置类型实现 [`CancelCondition`]，[`LiveView::cancel_token`] / [`LiveConfig::cancel_token`]
//! （`#[hy_live_config(cancel_condition)]` 生成的句柄上也有）得出一个子 [`CancellationToken`]，上级取消、或者按配置该取消了就取消
//!
//! ```ignore
//! #[derive(Serialize, Deserialize, hy_live_config)]
//! #[hy_live_config(cancel_condition)]               // 句柄上多一个 cancel_token，其余和不开时一样
//! pub struct JobSwitch {
//!     pub start: bool,
//!     #[serde(with = "hygiea::datetime::hms_range")]
//!     pub every_day_range: Range<Time>,
//! }
//!
//! impl CancelCondition for JobSwitch {
//!     fn should_cancel(&self) -> bool {
//!         !self.start || !self.every_day_range.contains(&now_time_local())
//!     }
//!
//!     // 配置不变也会到点失效：到时间段结束时再判断一次。只看开关这类条件不用写，默认只在配置变了时判断
//!     fn recheck_after(&self) -> Option<Duration> {
//!         Some(/* every_day_range.end - now_time_local() */)
//!     }
//! }
//!
//! // 跑一轮
//! let cancel = switch.cancel_token(&shutdown);      // CancelTokenGuard，这一轮结束前存着
//! match cancel.token().run_until_cancelled(work()).await {
//!     Some(result) => result,                       // 跑完了
//!     None => Ok(()),                               // 取消了：work 的 future 被丢掉
//! }
//! ```

use std::time::Duration;

use hygiea_core::app::CancellationToken;
use tokio::task::JoinHandle;

use super::{LiveConfig, LiveView};

/// 配置类型实现它：按这份配置，现在该不该取消。用在 [`LiveView::cancel_token`] 上。
/// 两个方法每次判断时一起调，读的是同一版配置；只读字段和当前时间这类，别做 IO
pub trait CancelCondition {
    /// 现在该不该取消。建 token 时、配置每变一次、每到 [`recheck_after`](Self::recheck_after) 就调一次
    fn should_cancel(&self) -> bool;

    /// 还不用取消时：配置不变、过多久结果也可能变（比如时间段到点），到时候再判断一次。
    /// 默认 `None`：只在配置变了时判断
    fn recheck_after(&self) -> Option<Duration> {
        None
    }
}

/// [`LiveView::cancel_token`] 返回：子 token 和盯着配置的后台任务。drop 掉后台任务就停，**不会取消 token**，
/// 之后 token 只跟着上级取消。要盯多久就存多久（比如一轮任务跑完前一直存着），写成 `let _ = ..` 会马上 drop
#[must_use = "drop 掉 guard 就不再按配置取消 token 了"]
#[derive(Debug)]
pub struct CancelTokenGuard {
    token: CancellationToken,
    /// 建的时候就该取消的没有后台任务
    task: Option<JoinHandle<()>>,
}

impl CancelTokenGuard {
    /// 子 token：上级取消、或者按配置该取消了时被取消
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for CancelTokenGuard {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl<U: CancelCondition + 'static> LiveView<U> {
    /// 从上级 token 得出一个子 token：上级取消、或者这一块配置的 [`CancelCondition::should_cancel`] 返回 `true` 时取消。
    /// 判断规则不只看配置时（比如还要看这一轮开始时的状态）用 [`cancel_token_with`](Self::cancel_token_with)。
    ///
    /// - 建的时候马上判断一次，已经该取消了就直接返回取消了的 token，不起后台任务
    /// - 否则内部 `tokio::spawn` 一个任务，配置变了（整份配置，同 [`changed`](Self::changed)）、或者过了
    ///   [`recheck_after`](CancelCondition::recheck_after) 就再判断一次，每次按判断时算出的 `recheck_after` 重新计时；
    ///   要在 tokio 运行时里调用（和 `tokio::spawn` 一样，不在运行时里会 panic）
    /// - 取消了、或者上级取消了任务就退出；配置组件关闭后（`changed()` 返回 [`RepoErr::LiveConfigClosed`](crate::RepoErr::LiveConfigClosed)）
    ///   不再等配置变化，只按 `recheck_after` 判断，没有 `recheck_after` 任务就退出
    /// - 返回的 [`CancelTokenGuard`] drop 掉任务就停，token 不会因此被取消
    pub fn cancel_token(&self, parent: &CancellationToken) -> CancelTokenGuard {
        self.cancel_token_with(parent, |config| {
            (config.should_cancel(), config.recheck_after())
        })
    }
}

impl<U: 'static> LiveView<U> {
    /// 同 [`cancel_token`](Self::cancel_token)，判断规则由调用方给：`condition` 按当前那一版配置返回
    /// （该不该取消，还不用时多久后再判断），含义同 [`CancelCondition`] 的两个方法。
    /// 配置类型不用实现 [`CancelCondition`]；规则要用到配置以外的状态时，放进闭包里：
    ///
    /// ```ignore
    /// // 这一轮开始时在时间段里，出了时间段就取消；开始时不在，进了时间段就取消
    /// let started_in_range = view.read(|c| c.range.contains(&now_time_local()));
    /// let cancel = view.cancel_token_with(&shutdown, move |c| {
    ///     (!c.start || c.range.contains(&now_time_local()) != started_in_range, Some(/* 离下一个边界多久 */))
    /// });
    /// ```
    pub fn cancel_token_with<F>(&self, parent: &CancellationToken, condition: F) -> CancelTokenGuard
    where
        F: Fn(&U) -> (bool, Option<Duration>) + Send + Sync + 'static,
    {
        let token = parent.child_token();
        let (should_cancel, recheck_after) = self.read(&condition);
        if should_cancel {
            token.cancel();
            return CancelTokenGuard { token, task: None };
        }
        let task = tokio::spawn(watch(self.clone(), token.clone(), recheck_after, condition));
        CancelTokenGuard {
            token,
            task: Some(task),
        }
    }
}

impl<T: CancelCondition + Send + Sync + 'static> LiveConfig<T> {
    /// 按整份配置得出子 token，见 [`LiveView::cancel_token`]
    pub fn cancel_token(&self, parent: &CancellationToken) -> CancelTokenGuard {
        self.view(|c| c).cancel_token(parent)
    }
}

impl<T: Send + Sync + 'static> LiveConfig<T> {
    /// 按整份配置得出子 token，判断规则由调用方给，见 [`LiveView::cancel_token_with`]
    pub fn cancel_token_with<F>(&self, parent: &CancellationToken, condition: F) -> CancelTokenGuard
    where
        F: Fn(&T) -> (bool, Option<Duration>) + Send + Sync + 'static,
    {
        self.view(|c| c).cancel_token_with(parent, condition)
    }
}

/// 后台任务：等上级取消、配置变化、`recheck_after` 到点，后两种按 `condition` 重新判断，该取消了就取消 token 退出。
/// 配置组件关闭了也重新判断一次（重新算 `recheck_after`），之后只按它到点判断
async fn watch<U: 'static, F: Fn(&U) -> (bool, Option<Duration>)>(
    mut view: LiveView<U>,
    token: CancellationToken,
    mut recheck_after: Option<Duration>,
    condition: F,
) {
    let mut closed = false;
    loop {
        if closed && recheck_after.is_none() {
            return;
        }
        let recheck = async {
            match recheck_after {
                Some(after) => tokio::time::sleep(after).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = token.cancelled() => return,
            changed = view.changed(), if !closed => closed = changed.is_err(),
            _ = recheck => {}
        }
        let (should_cancel, next_recheck_after) = view.read(&condition);
        recheck_after = next_recheck_after;
        if should_cancel {
            token.cancel();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::sync::watch;

    use super::*;

    #[derive(Clone, Debug, Default)]
    struct Root {
        name: String,
        switch: Switch,
    }

    /// `enabled` 关了取消；`recheck_after_ms` 不是 0 时到点再判断，`expired` 模拟配置没变、到点失效
    #[derive(Clone, Debug, Default)]
    struct Switch {
        enabled: bool,
        recheck_after_ms: u64,
        expired: Arc<AtomicBool>,
    }

    impl CancelCondition for Switch {
        fn should_cancel(&self) -> bool {
            !self.enabled || self.expired.load(Ordering::SeqCst)
        }

        fn recheck_after(&self) -> Option<Duration> {
            match self.recheck_after_ms {
                0 => None,
                ms => Some(Duration::from_millis(ms)),
            }
        }
    }

    /// 等不到时等多久算"不会发生"
    const WAIT: Duration = Duration::from_millis(50);

    fn enabled(recheck_after_ms: u64) -> Root {
        Root {
            name: String::new(),
            switch: Switch {
                enabled: true,
                recheck_after_ms,
                expired: Arc::default(),
            },
        }
    }

    /// 后台任务那一端：发新值、drop 掉模拟组件关闭
    fn channel(initial: Root) -> (watch::Sender<Arc<Root>>, LiveConfig<Root>) {
        let (sender, receiver) = watch::channel(Arc::new(initial));
        (sender, LiveConfig::new(receiver))
    }

    fn update(sender: &watch::Sender<Arc<Root>>, f: impl FnOnce(&mut Root)) {
        let mut next = (**sender.borrow()).clone();
        f(&mut next);
        sender.send_replace(Arc::new(next));
    }

    async fn cancelled_within(token: &CancellationToken, secs: u64) {
        tokio::time::timeout(Duration::from_secs(secs), token.cancelled())
            .await
            .expect("token 一直没取消");
    }

    async fn not_cancelled(token: &CancellationToken) {
        tokio::time::sleep(WAIT).await;
        assert!(!token.is_cancelled());
    }

    async fn task_finishes(guard: &CancelTokenGuard) {
        let task = guard.task.as_ref().expect("应该起了后台任务");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !task.is_finished() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("后台任务一直没退出");
    }

    /// 建的时候已经该取消：直接返回取消了的 token，不起后台任务
    #[tokio::test]
    async fn cancelled_at_creation() {
        let (_sender, config) = channel(Root::default());
        let parent = CancellationToken::new();
        let guard = config.view(|c| &c.switch).cancel_token(&parent);
        assert!(guard.token().is_cancelled());
        assert!(guard.task.is_none());
        assert!(!parent.is_cancelled(), "子 token 取消不影响上级");
    }

    /// 配置变了重新判断：别的字段变了不取消，盯的条件成立了取消，后台任务退出
    #[tokio::test]
    async fn config_change_cancels() {
        let (sender, config) = channel(enabled(0));
        let guard = config
            .view(|c| &c.switch)
            .cancel_token(&CancellationToken::new());
        update(&sender, |c| c.name = "x".to_string());
        not_cancelled(guard.token()).await;

        update(&sender, |c| c.switch.enabled = false);
        cancelled_within(guard.token(), 5).await;
        task_finishes(&guard).await;
    }

    /// 配置不变、到了 recheck_after 也重新判断
    #[tokio::test]
    async fn recheck_after_cancels_without_change() {
        let root = enabled(10);
        let expired = root.switch.expired.clone();
        let (_sender, config) = channel(root);
        let guard = config
            .view(|c| &c.switch)
            .cancel_token(&CancellationToken::new());
        not_cancelled(guard.token()).await;

        expired.store(true, Ordering::SeqCst);
        cancelled_within(guard.token(), 5).await;
    }

    /// 配置变了按新算出的 recheck_after 重新计时：原来要等很久，改短后按新的时长到点判断
    #[tokio::test]
    async fn config_change_resets_recheck() {
        let root = enabled(60_000);
        let expired = root.switch.expired.clone();
        let (sender, config) = channel(root);
        let guard = config
            .view(|c| &c.switch)
            .cancel_token(&CancellationToken::new());
        update(&sender, |c| c.switch.recheck_after_ms = 10);
        not_cancelled(guard.token()).await;

        expired.store(true, Ordering::SeqCst);
        cancelled_within(guard.token(), 2).await;
    }

    /// cancel_token_with：规则由调用方给，可以带上建 token 时的状态。这里是「开关和建的时候不一样了就取消」：
    /// 建的时候是关的也不马上取消（cancel_token 按 Switch 的规则会马上取消），打开后取消
    #[tokio::test]
    async fn cancel_token_with_uses_caller_condition() {
        let (sender, config) = channel(Root::default());
        let view = config.view(|c| &c.switch);
        let enabled_at_start = view.read(|switch| switch.enabled);
        let guard = view.cancel_token_with(&CancellationToken::new(), move |switch| {
            (switch.enabled != enabled_at_start, None)
        });
        assert!(!guard.token().is_cancelled());
        update(&sender, |c| c.name = "x".to_string());
        not_cancelled(guard.token()).await;

        update(&sender, |c| c.switch.enabled = true);
        cancelled_within(guard.token(), 5).await;
        task_finishes(&guard).await;
    }

    /// cancel_token_with 的 recheck_after 由调用方给：配置不变，到点也重新判断
    #[tokio::test]
    async fn cancel_token_with_rechecks() {
        let (_sender, config) = channel(Root::default());
        let expired = Arc::new(AtomicBool::new(false));
        let guard = config.cancel_token_with(&CancellationToken::new(), {
            let expired = expired.clone();
            move |_| {
                (
                    expired.load(Ordering::SeqCst),
                    Some(Duration::from_millis(10)),
                )
            }
        });
        not_cancelled(guard.token()).await;

        expired.store(true, Ordering::SeqCst);
        cancelled_within(guard.token(), 5).await;
    }

    /// 上级取消了子 token 跟着取消，后台任务退出
    #[tokio::test]
    async fn parent_cancel_cancels_child() {
        let (_sender, config) = channel(enabled(0));
        let parent = CancellationToken::new();
        let guard = config.view(|c| &c.switch).cancel_token(&parent);
        parent.cancel();
        assert!(guard.token().is_cancelled());
        task_finishes(&guard).await;
    }

    impl CancelCondition for Root {
        fn should_cancel(&self) -> bool {
            self.switch.should_cancel()
        }

        fn recheck_after(&self) -> Option<Duration> {
            self.switch.recheck_after()
        }
    }

    /// 整份配置实现 CancelCondition 时，LiveConfig 上直接调
    #[tokio::test]
    async fn live_config_cancel_token() {
        let (sender, config) = channel(enabled(0));
        let guard = config.cancel_token(&CancellationToken::new());
        not_cancelled(guard.token()).await;
        update(&sender, |c| c.switch.enabled = false);
        cancelled_within(guard.token(), 5).await;
    }

    /// drop 掉 guard 不再盯配置，token 也不会因此取消，之后只跟着上级
    #[tokio::test]
    async fn drop_guard_stops_watching() {
        let (sender, config) = channel(enabled(0));
        let parent = CancellationToken::new();
        let token = config
            .view(|c| &c.switch)
            .cancel_token(&parent)
            .token()
            .clone();
        assert!(!token.is_cancelled(), "drop guard 不取消 token");
        update(&sender, |c| c.switch.enabled = false);
        not_cancelled(&token).await;
        parent.cancel();
        assert!(token.is_cancelled());
    }

    /// 组件关闭后：有 recheck_after 的接着按它判断，没有的任务退出
    #[tokio::test]
    async fn closed_keeps_recheck() {
        let root = enabled(10);
        let expired = root.switch.expired.clone();
        let (sender, config) = channel(root);
        let view = config.view(|c| &c.switch);
        let rechecking = view.cancel_token(&CancellationToken::new());
        drop(sender);
        not_cancelled(rechecking.token()).await;
        expired.store(true, Ordering::SeqCst);
        cancelled_within(rechecking.token(), 5).await;

        let (sender, config) = channel(enabled(0));
        let guard = config
            .view(|c| &c.switch)
            .cancel_token(&CancellationToken::new());
        drop(sender);
        task_finishes(&guard).await;
        assert!(!guard.token().is_cancelled());
    }

    /// 固定值的配置：changed() 一直等不到，只靠 recheck_after
    #[tokio::test]
    async fn fixed_rechecks() {
        let root = enabled(10);
        let expired = root.switch.expired.clone();
        let config = LiveConfig::fixed(root);
        let guard = config
            .view(|c| &c.switch)
            .cancel_token(&CancellationToken::new());
        not_cancelled(guard.token()).await;
        expired.store(true, Ordering::SeqCst);
        cancelled_within(guard.token(), 5).await;
    }

    /// 外层不开 cancel_condition，里层开：外层句柄照常，里层句柄多一个 cancel_token
    #[derive(Clone, Debug, Default, crate::live_config::hy_live_config)]
    struct DerivedRoot {
        name: String,
        #[live_config(nested)]
        switch: DerivedSwitch,
    }

    #[derive(Clone, Debug, Default, crate::live_config::hy_live_config)]
    #[hy_live_config(cancel_condition)]
    struct DerivedSwitch {
        enabled: bool,
    }

    /// 只看开关，不写 recheck_after
    impl CancelCondition for DerivedSwitch {
        fn should_cancel(&self) -> bool {
            !self.enabled
        }
    }

    #[tokio::test]
    async fn derive_nested_cancel_token() {
        let (sender, receiver) = watch::channel(Arc::new(DerivedRoot {
            name: String::new(),
            switch: DerivedSwitch { enabled: true },
        }));
        let config = LiveConfig::new(receiver);
        let root = config.fields();
        let switch = root.switch();
        let guard = switch.cancel_token(&CancellationToken::new());
        // 开了 cancel_condition 的句柄，原来的字段方法、snapshot 照常
        assert!(switch.enabled());
        assert!(switch.snapshot().enabled);

        let mut next = (**sender.borrow()).clone();
        next.name = "x".to_string();
        sender.send_replace(Arc::new(next));
        not_cancelled(guard.token()).await;
        assert_eq!(root.name(), "x");

        let mut next = (**sender.borrow()).clone();
        next.switch.enabled = false;
        sender.send_replace(Arc::new(next));
        cancelled_within(guard.token(), 5).await;
    }
}
