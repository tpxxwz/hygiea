// 字段上未知的 live 属性
use hygiea::db::live_config::Live;

#[derive(Live)]
struct Job {
    #[live(copy)]
    times: u32,
}

fn main() {}
