// 字段方法的可见性和字段相同：私有字段的方法在模块外调不到
mod config {
    use hygiea::db::live_config::hy_live_config;

    #[derive(hy_live_config)]
    pub struct Job {
        pub enabled: bool,
        secret: String,
    }
}

#[allow(dead_code)]
fn read(job: &config::JobLiveConfig) {
    let _ = job.enabled();
    let _ = job.secret();
}

fn main() {}
