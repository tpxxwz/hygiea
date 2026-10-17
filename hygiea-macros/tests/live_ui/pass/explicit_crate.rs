// crate = ".." 写 facade 的路径（这里经别名转一手）
use hygiea::db::live_config::Live;

mod reexport {
    pub use hygiea as my_hygiea;
}

#[derive(Live)]
#[live(crate = "crate::reexport::my_hygiea")]
struct Job {
    times: u32,
}

fn main() {
    let job = hygiea::db::live_config::LiveConfig::fixed(Job { times: 1 }).live();
    let _: u32 = job.times();
}
