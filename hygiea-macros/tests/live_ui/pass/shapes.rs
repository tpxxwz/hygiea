// 三层嵌套、by_ref、nested、带文档的字段、pub(crate)，句柄能 Clone
use hygiea::db::live_config::{Live, LiveConfig, LiveRef};

#[derive(Live)]
pub struct Root {
    /// 名字
    pub name: String,
    #[live(nested)]
    pub job: Job,
}

#[derive(Live)]
pub struct Job {
    pub enabled: bool,
    #[live(by_ref)]
    pub levels: Vec<i32>,
    #[live(nested)]
    pub(crate) retry: Retry,
}

#[derive(Live)]
pub(crate) struct Retry {
    pub times: u32,
}

fn main() {
    let config = LiveConfig::fixed(Root {
        name: String::new(),
        job: Job {
            enabled: false,
            levels: Vec::new(),
            retry: Retry { times: 0 },
        },
    });
    let root: RootLive = config.live();
    let job: JobLive = root.job().clone();
    let _: String = root.name();
    let _: bool = job.enabled();
    let _: LiveRef<Vec<i32>> = job.levels();
    let _: u32 = job.retry().times();
    let _: LiveRef<Job> = job.snapshot();
}
