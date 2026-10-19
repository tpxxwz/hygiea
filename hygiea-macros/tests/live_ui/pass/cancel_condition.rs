// cancel_condition：句柄多一个 cancel_token，原来的方法照常；放进不开的外层当 nested 字段，外层照常
use hygiea::app::CancellationToken;
use hygiea::db::live_config::{hy_live_config, CancelCondition, CancelTokenGuard, LiveConfig, LiveRef};

#[derive(hy_live_config)]
pub struct Root {
    pub name: String,
    #[live_config(nested)]
    pub switch: Switch,
}

#[derive(hy_live_config)]
#[hy_live_config(cancel_condition)]
pub struct Switch {
    pub enabled: bool,
}

impl CancelCondition for Switch {
    fn should_cancel(&self) -> bool {
        !self.enabled
    }
}

fn main() {
    let config = LiveConfig::fixed(Root {
        name: String::new(),
        switch: Switch { enabled: false },
    });
    let root: RootLiveConfig = config.fields();
    let _: String = root.name();
    let switch: SwitchLiveConfig = root.switch();
    let _: bool = switch.enabled();
    let _: LiveRef<Switch> = switch.snapshot();
    // 已经该取消了：直接返回取消了的 token，不起后台任务，不需要 tokio 运行时
    let guard: CancelTokenGuard = switch.cancel_token(&CancellationToken::new());
    assert!(guard.token().is_cancelled());
}
