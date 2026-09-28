//! 配置加载：`Registry::load_config` 按下面的顺序合并，后面的覆盖前面的同名字段：
//!
//! 1. `<配置目录>/<env>.toml`：环境名取 `--env`，没有就取 `HYGIEA_ENV`，再没有就是 `dev`
//! 2. `-f` 指定的额外文件，按顺序
//! 3. 环境变量 `HYGIEA__<层级>__<字段>`，比如 `HYGIEA__GREETING=hi`
//! 4. `--set key=value`，可以写多次
//!
//! 同时演示把框架的参数（`ConfigArgs`）合进应用自己的命令行。配置文件在 `hygiea-examples/config/app_config/`，用 `ConfigArgs::default_config_dir` 指定，也可以用 `-d` 换。
//!
//! ```bash
//! cargo run -p hygiea-examples --example app_config
//! cargo run -p hygiea-examples --example app_config -- --env prod
//! cargo run -p hygiea-examples --example app_config -- -f local --set greeting=hey
//! HYGIEA__GREETING=hi cargo run -p hygiea-examples --example app_config
//! cargo run -p hygiea-examples --example app_config -- --dry-run
//! cargo run -p hygiea-examples --example app_config -- --help
//! ```
//!
//! 启动时会先打出本次读了哪些文件、哪些环境变量名、哪些 `--set` 的 key（不打值，值里可能有密码）。

use hygiea::HyErr;
use hygiea::app::{ConfigArgs, IntoRegistryConfig, Registry, RegistryConfig, clap};
use hygiea::db::{SqlxSqliteComponent, SqlxSqliteConfig};
use serde::Deserialize;

/// 应用的顶层配置：框架的配置在 `[registry]` 段，组件和业务的配置各占一段
#[derive(Deserialize)]
struct AppConfig {
    registry: RegistryConfig,
    db: SqlxSqliteConfig,
    greeting: String,
}

impl IntoRegistryConfig for AppConfig {
    fn registry_config(&self) -> RegistryConfig {
        self.registry.clone()
    }
}

/// 应用自己的命令行：框架的 `--env` / `-f` / `--set` 加上自己的参数
#[derive(clap::Parser)]
#[command(version, about = "hygiea config example")]
struct Cli {
    #[command(flatten)]
    config: ConfigArgs,
    /// 只打印配置，不启动组件
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<(), HyErr> {
    let cli = <Cli as clap::Parser>::parse();
    // 配置目录默认是相对当前目录的 config/，这里换成绝对路径，从哪里 cargo run 都能找到
    let args = cli
        .config
        .default_config_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/config/app_config"));
    // 配置读不到或解析失败是启动期错误：把读了什么、错在哪打到 stderr 后 panic
    let (registry, config) = Registry::load_config::<AppConfig>(&args);

    tracing::info!(
        "greeting = {:?}, db.database = {:?}, shutdown_timeout_secs = {}",
        config.greeting,
        config.db.database,
        config.registry.shutdown_timeout_secs
    );
    if cli.dry_run {
        return Ok(());
    }

    let (result, _log_guard) = registry
        .add::<SqlxSqliteComponent>(config.db)
        .on_ready(|_| async {
            tracing::info!("press Ctrl+C to exit");
            Ok(())
        })
        .run()
        .await;
    result
}
