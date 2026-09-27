//! 全局状态：组件都启动后，在 `run` 的闭包里把要用的资源收进一个全局 `AppState`，
//! 业务代码直接取，不用一路传 `Resources`。
//!
//! ```bash
//! cargo run -p hygiea-examples --example app_global_state
//! ```
//!
//! 启动后按 Ctrl+C 退出。

use std::sync::OnceLock;

use hygiea::app::Registry;
use hygiea::db::{SqlxSqliteComponent, SqlxSqliteConfig, SqlxSqlitePool};
use hygiea::{BaseErr, HyErr, err};

// ---- 全局状态 ----

pub struct AppState {
    pub primary: SqlxSqlitePool,
    pub replica: SqlxSqlitePool,
}

static APP_STATE: OnceLock<AppState> = OnceLock::new();

impl AppState {
    /// 只在 run 的闭包里调一次
    fn init(state: AppState) -> Result<(), HyErr> {
        APP_STATE.set(state).map_err(|_| {
            err!(BaseErr::SysErr).with_source(std::io::Error::other("AppState already initialized"))
        })
    }

    /// 没初始化就返回错误，不 panic
    pub fn get() -> Result<&'static AppState, HyErr> {
        APP_STATE.get().ok_or_else(|| {
            err!(BaseErr::SysErr).with_source(std::io::Error::other("AppState not initialized"))
        })
    }
}

// ---- 业务代码：直接取全局状态 ----

fn report() -> Result<(), HyErr> {
    let state = AppState::get()?;
    tracing::info!(
        "primary: {}, replica: {}",
        state.primary.config().database,
        state.replica.config().database
    );
    Ok(())
}

fn memory_db() -> SqlxSqliteConfig {
    SqlxSqliteConfig {
        database: ":memory:".to_string(),
        ..Default::default()
    }
}

#[tokio::main]
async fn main() -> Result<(), HyErr> {
    let registry = Registry::new()
        .add_named::<SqlxSqliteComponent>("primary", memory_db())
        .add_named::<SqlxSqliteComponent>("replica", memory_db());

    let (result, _log_guard) = registry
        .run(|resources| async move {
            AppState::init(AppState {
                primary: resources.require_named::<SqlxSqlitePool>("primary")?,
                replica: resources.require_named::<SqlxSqlitePool>("replica")?,
            })?;

            report()?;
            tracing::info!("press Ctrl+C to exit");
            Ok(())
        })
        .await;
    result
}
