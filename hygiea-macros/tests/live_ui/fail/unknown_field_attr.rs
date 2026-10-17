// 字段上未知的 live_config 属性
use hygiea::db::live_config::hy_live_config;

#[derive(hy_live_config)]
struct Job {
    #[live_config(copy)]
    times: u32,
}

fn main() {}
