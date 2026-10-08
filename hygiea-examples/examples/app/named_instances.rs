//! 同一种组件注册多个实例：主库、从库各一个 SQLite 连接池，按名字区分。
//!
//! ```bash
//! cargo run -p hygiea-examples --example app_named_instances
//! ```
//!
//! 启动后按 Ctrl+C 退出。

use hygiea::Result;
use hygiea::app::Registry;
use hygiea::db::{SqlxSqliteComponent, SqlxSqliteConfig, SqlxSqlitePool};

fn memory_db() -> SqlxSqliteConfig {
    SqlxSqliteConfig {
        database: ":memory:".to_string(),
        ..Default::default()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 同一个组件类型用不同的名字 add 两次；同类型、同名字重复 add 会 panic
    let registry = Registry::new()
        .add_named::<SqlxSqliteComponent>("primary", memory_db())
        .add_named::<SqlxSqliteComponent>("replica", memory_db());

    let (result, _log_guard) = registry
        .before_activate(|resources| async move {
            // 组件把连接池按自己的名字放进 Resources，取的时候带上名字
            let primary = resources.require_named::<SqlxSqlitePool>("primary")?;
            let replica = resources.require_named::<SqlxSqlitePool>("replica")?;
            tracing::info!(
                "primary pool size: {}, replica pool size: {}",
                primary.size(),
                replica.size()
            );

            // 没有注册过的名字取不到，返回错误而不是 panic
            if let Err(e) = resources.require_named::<SqlxSqlitePool>("analytics") {
                tracing::warn!("expected failure: {e:#}");
            }
            Ok(())
        })
        .on_ready(|_| async {
            tracing::info!("press Ctrl+C to exit");
            Ok(())
        })
        .run()
        .await;
    result
}
