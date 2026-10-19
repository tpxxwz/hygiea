//! 经 facade 的 `#[derive(hy_live_config)]`，断言生成的方法读到的值（不只是能编译）。值变了之后读到新值的测试在
//! hygiea-db 的 `live_config/handle.rs`（要直接往 watch channel 里发新值）和 container 层的 `tests/sqlx_postgres.rs`

use hygiea::app::CancellationToken;
use hygiea::db::live_config::{
    CancelCondition, LiveConfig, LiveConfigFields, LiveRef, hy_live_config,
};

#[derive(Debug, PartialEq, hy_live_config)]
pub struct Root {
    pub name: String,
    #[live_config(nested)]
    pub job: Job,
    /// Root 不开 cancel_condition，Switch 开
    #[live_config(nested)]
    pub switch: Switch,
}

#[derive(Debug, PartialEq, hy_live_config)]
pub struct Job {
    pub enabled: bool,
    #[live_config(by_ref)]
    pub levels: Vec<i32>,
    #[live_config(nested)]
    pub(crate) retry: Retry,
}

#[derive(Debug, PartialEq, hy_live_config)]
pub(crate) struct Retry {
    pub times: u32,
    /// 私有字段：生成的方法也是私有的，同一个模块里能调
    secret: String,
}

#[derive(Debug, PartialEq, hy_live_config)]
#[hy_live_config(cancel_condition)]
pub struct Switch {
    pub enabled: bool,
}

impl CancelCondition for Switch {
    fn should_cancel(&self) -> bool {
        !self.enabled
    }
}

fn config() -> LiveConfig<Root> {
    LiveConfig::fixed(Root {
        name: "root".to_string(),
        job: Job {
            enabled: true,
            levels: vec![1, 2],
            retry: Retry {
                times: 3,
                secret: "s".to_string(),
            },
        },
        switch: Switch { enabled: true },
    })
}

/// 三层：字段方法、nested 往下、by_ref 和 snapshot
#[test]
fn field_methods_read_values() {
    let root = config().fields();
    assert_eq!(root.name(), "root");
    let job = root.job();
    assert!(job.enabled());
    let levels: LiveRef<Vec<i32>> = job.levels();
    assert_eq!(*levels, vec![1, 2]);
    let retry = job.retry();
    assert_eq!(retry.times(), 3);
    assert_eq!(retry.secret(), "s");
    assert_eq!(job.snapshot().retry.times, 3);
    assert_eq!(root.snapshot().job.levels, vec![1, 2]);
}

/// 句柄 Clone 后各处共用；也能从 LiveView 拿
#[test]
fn handle_is_clone_and_from_view() {
    let config = config();
    let job = config.view(|c| &c.job).fields();
    let cloned = job.clone();
    assert_eq!(cloned.retry().times(), 3);
    let retry: RetryLiveConfig = <Retry as LiveConfigFields>::handle(config.view(|c| &c.job.retry));
    assert_eq!(retry.times(), 3);
}

/// cancel_condition：句柄多一个 cancel_token，按当前的值判断，字段方法照常；放在不开的 Root 里当 nested 字段
#[tokio::test]
async fn cancel_condition_handle() {
    let switch = config().fields().switch();
    assert!(switch.enabled());
    let guard = switch.cancel_token(&CancellationToken::new());
    assert!(!guard.token().is_cancelled());

    let off = LiveConfig::fixed(Switch { enabled: false });
    let guard = off.fields().cancel_token(&CancellationToken::new());
    assert!(guard.token().is_cancelled());
}
