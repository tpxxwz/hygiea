// 开了 cancel_condition 时字段名和生成的方法 cancel_token 重名
use hygiea::db::live_config::{hy_live_config, CancelCondition};

#[derive(hy_live_config)]
#[hy_live_config(cancel_condition)]
struct Job {
    cancel_token: bool,
}

impl CancelCondition for Job {
    fn should_cancel(&self) -> bool {
        true
    }
}

fn main() {}
