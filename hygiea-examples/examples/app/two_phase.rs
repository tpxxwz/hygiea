//! 两阶段启动：`before_activate` 在 HTTP 开始接请求之前执行，适合建表、预热、初始化全局状态。
//!
//! 1. 第一阶段：SQLite 连接池建好；HTTP 只绑端口，不接请求
//! 2. `before_activate`：建表、写入初始数据，用连接池组装 `AppState`，放进 Resources
//! 3. 第二阶段：HTTP 调 router 函数，从 Resources 取出 `AppState` 交给 `with_state`，开始接请求；
//!    handler 用 `arity0_state` 拿到它
//! 4. `on_ready`：服务已经在接请求，适合注册到注册中心、打"已就绪"的点
//!
//! router 在资源都就绪后才建，所以不需要全局变量，handler 拿到的 state 一定是初始化好的。
//!
//! ```bash
//! cargo run -p hygiea-examples --example app_two_phase
//! curl http://127.0.0.1:8081/users
//! ```
//!
//! 启动后按 Ctrl+C 退出，日志里能看到顺序：SQLite 启动 → HTTP 绑端口 → before_activate → HTTP 开始服务 → on_ready。

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use hygiea::app::Registry;
use hygiea::db::{SqlxSqliteComponent, SqlxSqliteConfig, SqlxSqlitePool};
use hygiea::http::{AxumComponent, AxumConfig, arity0_state};
use hygiea::{BaseErr, Result, ResultExt, err};

// ---- 应用状态：handler 用到的资源 ----

pub struct AppState {
    pub db: SqlxSqlitePool,
}

// ---- before_activate 里做的初始化 ----

/// 建表、写入初始数据，相当于迁移和预热
async fn migrate(db: &SqlxSqlitePool) -> Result<()> {
    sqlx::query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .execute(&db.inner)
        .await
        .wrap_err(|| err!(BaseErr::SysErr))?;
    for name in ["alice", "bob"] {
        sqlx::query("INSERT INTO users (name) VALUES (?)")
            .bind(name)
            .execute(&db.inner)
            .await
            .wrap_err(|| err!(BaseErr::SysErr))?;
    }
    Ok(())
}

// ---- handler：从 router 的 state 取连接池 ----

async fn list_users(state: Arc<AppState>) -> Result<Vec<String>> {
    let db = &state.db;
    let names: Vec<(String,)> = sqlx::query_as("SELECT name FROM users ORDER BY id")
        .fetch_all(&db.inner)
        .await
        .wrap_err(|| err!(BaseErr::SysErr))?;
    Ok(names.into_iter().map(|(n,)| n).collect())
}

#[tokio::main]
async fn main() -> Result<()> {
    let db = SqlxSqliteConfig {
        database: ":memory:".to_string(),
        // 内存库每个连接各是一个库，只留一个连接，建的表 handler 才看得到
        max_connections: Some(1),
        min_connections: Some(1),
        ..Default::default()
    };
    let http = AxumConfig {
        host: "127.0.0.1".to_string(),
        port: 8081,
        // activate 时才调用：before_activate 已经把 AppState 放进 Resources 了
        router: Some(Box::new(|resources| {
            let state = resources.require::<Arc<AppState>>()?;
            Ok(Router::new()
                .route("/users", get(arity0_state(list_users)))
                .with_state(state))
        })),
        ..Default::default()
    };

    let (result, _log_guard) = Registry::new()
        .add::<SqlxSqliteComponent>(db)
        .add::<AxumComponent>(http)
        .before_activate(|resources| async move {
            // 这时 SQLite 已经连上、HTTP 已经绑好端口，但还没开始接请求
            let db = resources.require::<SqlxSqlitePool>()?;
            migrate(&db).await?;
            resources.insert(Arc::new(AppState { db }));
            tracing::info!("init done, HTTP starts serving next");
            Ok(())
        })
        .on_ready(|_| async {
            // 这时 HTTP 已经在接请求
            tracing::info!("serving, try: curl http://127.0.0.1:8081/users");
            Ok(())
        })
        .run()
        .await;
    result
}
