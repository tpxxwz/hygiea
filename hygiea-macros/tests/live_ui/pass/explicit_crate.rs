// crate = ".." 写 facade 的路径（这里经别名转一手）
use hygiea::db::live_config::hy_live_config;

mod reexport {
    pub use hygiea as my_hygiea;
}

#[derive(hy_live_config)]
#[hy_live_config(crate = "crate::reexport::my_hygiea")]
struct Job {
    times: u32,
}

fn main() {
    let job = hygiea::db::live_config::LiveConfig::fixed(Job { times: 1 }).fields();
    let _: u32 = job.times();
}
