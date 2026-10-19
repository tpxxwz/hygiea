// 不开 cancel_condition 的句柄上没有 cancel_token
use hygiea::app::CancellationToken;
use hygiea::db::live_config::{hy_live_config, LiveConfig};

#[derive(hy_live_config)]
struct Job {
    enabled: bool,
}

fn main() {
    let config = LiveConfig::fixed(Job { enabled: true });
    let _ = config.fields().cancel_token(&CancellationToken::new());
}
