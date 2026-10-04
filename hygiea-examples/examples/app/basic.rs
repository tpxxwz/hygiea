//! 最小的应用：现成的 SQLite 和 HTTP 组件，配置自动从 `hygiea-examples/config/app_basic/dev.toml` 加载。
//!
//! ```bash
//! cargo run -p hygiea-examples --example app_basic
//! curl http://127.0.0.1:8080/hello
//! ```
//!
//! 按 Ctrl+C 退出，组件按启动的逆序关闭。

use axum::Router;
use axum::routing::get;
use hygiea::Result;
use hygiea::app::{ConfigArgs, IntoRegistryConfig, Registry, RegistryConfig};
use hygiea::db::{SqlxSqliteComponent, SqlxSqliteConfig};
use hygiea::http::{AxumComponent, AxumConfig};
use serde::Deserialize;

/// 对应 config/app_basic/dev.toml：框架的配置在 [registry] 段，每个组件的配置各占一段
#[derive(Deserialize)]
struct AppConfig {
    registry: RegistryConfig,
    db: SqlxSqliteConfig,
    http: AxumConfig,
}

impl IntoRegistryConfig for AppConfig {
    fn registry_config(&self) -> RegistryConfig {
        self.registry.clone()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 读 <配置目录>/<env>.toml（默认 dev），再叠加 -f 指定的文件、HYGIEA__ 环境变量、--set。
    // 配置目录用绝对路径，从哪里 cargo run 都能找到
    let args = ConfigArgs::from_cli()
        .default_config_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/config/app_basic"));
    let (registry, mut config) = Registry::load_config::<AppConfig>(&args);
    config.http.router = Some(Router::new().route("/hello", get(|| async { "hello\n" })));

    let (result, _log_guard) = registry
        .add::<SqlxSqliteComponent>(config.db)
        .add::<AxumComponent>(config.http)
        .run()
        .await;
    result
}
