//! 两阶段启动：`before_activate` 在 HTTP 开始接请求之前执行，适合建表、预热、初始化全局状态。
//!
//! 1. 第一阶段：SQLite 连接池建好；HTTP 只绑端口，不接请求
//! 2. `before_activate`：建表、写入初始数据，把连接池收进全局 `AppState`
//! 3. 第二阶段：HTTP 开始接请求，handler 从 `AppState` 取连接池
//! 4. `on_ready`：服务已经在接请求，适合注册到注册中心、打"已就绪"的点
//!
//! 请求进来时 `AppState` 一定已经初始化好了，handler 里不会遇到"还没初始化"。
//!
//! ```bash
//! cargo run -p hygiea-examples --example app_two_phase
//! curl http://127.0.0.1:8081/users
//! ```
//!
//! 启动后按 Ctrl+C 退出，日志里能看到顺序：SQLite 启动 → HTTP 绑端口 → before_activate → HTTP 开始服务 → on_ready。

use std::sync::OnceLock;

use axum::Router;
use axum::routing::get;
use hygiea::app::Registry;
use hygiea::db::{SqlxSqliteComponent, SqlxSqliteConfig, SqlxSqlitePool};
use hygiea::http::{AxumComponent, AxumConfig, arity0};
use hygiea::{BaseErr, HyErr, ResultExt, err};

// ---- 全局状态 ----

pub struct AppState {
    pub db: SqlxSqlitePool,
}

static APP_STATE: OnceLock<AppState> = OnceLock::new();

impl AppState {
    /// 只在 before_activate 里调一次
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

// ---- before_activate 里做的初始化 ----

/// 建表、写入初始数据，相当于迁移和预热
async fn migrate(db: &SqlxSqlitePool) -> Result<(), HyErr> {
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

// ---- handler：从全局状态取连接池 ----

async fn list_users() -> Result<Vec<String>, HyErr> {
    let db = &AppState::get()?.db;
    let names: Vec<(String,)> = sqlx::query_as("SELECT name FROM users ORDER BY id")
        .fetch_all(&db.inner)
        .await
        .wrap_err(|| err!(BaseErr::SysErr))?;
    Ok(names.into_iter().map(|(n,)| n).collect())
}

#[tokio::main]
async fn main() -> Result<(), HyErr> {
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
        // router 在 add 之前就写好，handler 用到的资源到请求进来时再从 AppState 取
        router: Some(Router::new().route("/users", get(arity0(list_users)))),
        ..Default::default()
    };

    let (result, _log_guard) = Registry::new()
        .add::<SqlxSqliteComponent>(db)
        .add::<AxumComponent>(http)
        .before_activate(|resources| async move {
            // 这时 SQLite 已经连上、HTTP 已经绑好端口，但还没开始接请求
            let db = resources.require::<SqlxSqlitePool>()?;
            migrate(&db).await?;
            AppState::init(AppState { db })?;
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
