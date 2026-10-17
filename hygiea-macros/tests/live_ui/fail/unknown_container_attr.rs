// struct 上的 hy_live_config 属性只能写 crate = ".."
use hygiea::db::live_config::hy_live_config;

#[derive(hy_live_config)]
#[hy_live_config(rename_all = "snake_case")]
struct Job {
    times: u32,
}

fn main() {}
