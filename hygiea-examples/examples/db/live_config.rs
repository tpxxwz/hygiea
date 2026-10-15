//! 动态配置（数据库配置表）：配置存在库里，改了表，运行中的程序自动拿到新值。
//!
//! 1. 第一阶段：SQLite 连接池组件 `uses = 3`（1 业务池 + 2 动态配置），建好业务池、把 `DbLiveConfig` 放进 Resources
//!    （SQLite 默认 1 秒轮询一次）
//! 2. `before_activate`：`DbLiveConfig::bind_all(..)` 第一次绑定时建表、把默认值写进表（表里已有的 key 不动），
//!    读出来转成 `RuntimeConfig`，放进 `AppState`
//! 3. handler 每次 `get()` 拿当前最新的值；另起一个任务等 `changed()`，变了打日志
//! 4. 改表（这里用 `/admin/config` 接口模拟别的程序、手写 SQL 改表），一个轮询间隔内 handler 看到新值
//!
//! ```bash
//! cargo run -p hygiea-examples --example db_live_config
//! curl http://127.0.0.1:8082/greeting
//! curl -X POST http://127.0.0.1:8082/admin/config -H 'content-type: application/json' \
//!      -d '{"namespace":"greeting","key":"text","value":"Hi"}'
//! curl http://127.0.0.1:8082/greeting   # 1 秒内变成 Hi
//! curl -X POST http://127.0.0.1:8082/admin/config -H 'content-type: application/json' \
//!      -d '{"namespace":"feature","key":"shout","value":true}'
//! ```
//!
//! 库文件在系统临时目录（启动日志里有路径），再启动一次时表里的值还在，默认值不会把它覆盖回去。
//! 启动时框架按 namespace 打出生效的值，之后每次变更打出改了哪些 key（旧值 -> 新值）；`webhook_token` 标了
//! `#[redact(mask)]`，日志里是 `***`：
//!
//! ```bash
//! curl -X POST http://127.0.0.1:8082/admin/config -H 'content-type: application/json' \
//!      -d '{"namespace":"feature","key":"webhook_token","value":"abc"}'
//! ```
//!
//! 表名前缀是应用名（这里没配，默认 `hygiea`），配置表是 `hygiea_config`。
//! 表里还有 `hygiea_config_revision`（每个 namespace 的版本号）和 `hygiea_config_history`（每次变更的新旧值），由触发器维护。

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use hygiea::app::Registry;
use hygiea::db::{
    DbLiveConfig, DbUses, LiveConfig, SqliteConnectConfig, SqlxSqliteComponent, SqlxSqliteConfig,
    SqlxSqlitePool,
};
use hygiea::http::{AxumComponent, AxumComponentConfig, AxumConfig, arity0_state, arity1_state};
use hygiea::redact::redact;
use hygiea::{BaseErr, Result, ResultExt, err};
use serde::{Deserialize, Serialize};

// ---- 动态配置：每个字段是一个 namespace，再下一层是 key ----

#[derive(Serialize, Deserialize, Default)]
pub struct RuntimeConfig {
    pub greeting: Greeting,
    pub feature: Feature,
}

#[derive(Serialize, Deserialize)]
pub struct Greeting {
    pub text: String,
    pub name: String,
}

/// 表里没有的 key 用这里的值
impl Default for Greeting {
    fn default() -> Self {
        Self {
            text: "Hello".to_string(),
            name: "hygiea".to_string(),
        }
    }
}

/// 标了 `#[redact(mask)]` 的字段，框架打的配置日志里显示成 `***`
#[redact]
#[derive(Serialize, Deserialize, Default)]
pub struct Feature {
    pub shout: bool,
    #[redact(mask)]
    pub webhook_token: String,
}

// ---- 应用状态 ----

pub struct AppState {
    pub db: SqlxSqlitePool,
    pub runtime: LiveConfig<RuntimeConfig>,
}

// ---- handler ----

/// 每次请求都 get() 一次，拿到的是当前最新的配置
async fn greeting(state: Arc<AppState>) -> Result<String> {
    let cfg = state.runtime.get();
    let text = format!("{}, {}!", cfg.greeting.text, cfg.greeting.name);
    Ok(if cfg.feature.shout {
        text.to_uppercase()
    } else {
        text
    })
}

#[derive(Deserialize)]
struct SetConfig {
    namespace: String,
    key: String,
    value: serde_json::Value,
}

/// 模拟别的程序改表：就是一条普通的 UPDATE，动态配置那边靠轮询发现
async fn set_config(state: Arc<AppState>, req: SetConfig) -> Result<String> {
    sqlx::query("UPDATE hygiea_config SET value = ?1 WHERE namespace = ?2 AND key = ?3")
        .bind(req.value.to_string())
        .bind(&req.namespace)
        .bind(&req.key)
        .execute(&*state.db)
        .await
        .wrap_err(|| err!(BaseErr::SysErr))?;
    Ok("updated".to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::temp_dir().join("hygiea_live_config_example.db");
    let db = SqlxSqliteConfig {
        // 动态配置另开一条连接，要用文件库（内存库每个连接各是一个库）
        connect: SqliteConnectConfig {
            database: path.display().to_string(),
            ..Default::default()
        },
        // 业务池 + 动态配置，配置文件里写 uses = 3；[db.live_config] 段不写用默认值
        uses: DbUses::try_from(DbUses::POOL | DbUses::LIVE_CONFIG)?,
        ..Default::default()
    };
    let http = AxumComponentConfig {
        config: AxumConfig {
            host: "127.0.0.1".to_string(),
            port: 8082,
            ..Default::default()
        },
        router: Box::new(|resources| {
            let state = resources.require::<Arc<AppState>>()?;
            Ok(Router::new()
                .route("/greeting", get(arity0_state(greeting)))
                .route("/admin/config", post(arity1_state(set_config)))
                .with_state(state))
        }),
    };

    let (result, _log_guard) = Registry::new()
        .add::<SqlxSqliteComponent>(db)
        .add::<AxumComponent>(http)
        .before_activate(move |resources| async move {
            let db = resources.require::<SqlxSqlitePool>()?;
            // 第一次 bind：建表、补默认值、加载；失败的话启动失败
            let runtime = resources
                .require::<DbLiveConfig>()?
                .bind_all(RuntimeConfig::default())
                .await?;
            tracing::info!("live config loaded from {}", path.display());

            // 配置变了要做点什么（比如定时任务改了时间要重新排期）就等 changed()
            let mut watcher = runtime.clone();
            tokio::spawn(async move {
                while watcher.changed().await.is_ok() {
                    let cfg = watcher.get();
                    tracing::info!(
                        text = cfg.greeting.text,
                        shout = cfg.feature.shout,
                        "live config changed"
                    );
                }
            });

            resources.insert(Arc::new(AppState { db, runtime }));
            Ok(())
        })
        .on_ready(|_| async {
            tracing::info!("serving, try: curl http://127.0.0.1:8082/greeting");
            Ok(())
        })
        .run()
        .await;
    result
}
