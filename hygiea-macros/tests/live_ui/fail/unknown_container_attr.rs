// struct 上的 live 属性只能写 crate = ".."
use hygiea::db::live_config::Live;

#[derive(Live)]
#[live(rename_all = "snake_case")]
struct Job {
    times: u32,
}

fn main() {}
