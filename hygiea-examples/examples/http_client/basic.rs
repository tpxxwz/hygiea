//! HTTP 客户端：本进程内起一个 axum 服务（随机端口，不连外部服务），用 hygiea 的 http 客户端请求它。
//! 控制台日志能看到请求参数里标了 `#[redact(mask)]` 的字段被打码，`#[redact(skip)]` 的字段不出现。
//!
//! ```bash
//! cargo run -p hygiea-examples --example http_client_basic
//! ```

use axum::Router;
use axum::routing::post;
use hygiea::http_client::reqwest_client::{Json, Method, RequestConfig, ReqwestConfig};
use hygiea::redact::redact;
use hygiea::{BaseErr, HyErr, err};
use serde::{Deserialize, Serialize};

/// 请求参数：password 打码显示成 "***"，device_id 直接不出现在日志里，username 照常显示
#[redact]
#[derive(Serialize, Deserialize, Clone)]
struct LoginParams {
    username: String,
    #[redact(mask)]
    password: String,
    #[redact(skip)]
    device_id: String,
}

#[derive(Serialize, Deserialize)]
struct LoginResult {
    token: String,
}

async fn login(axum::Json(params): axum::Json<LoginParams>) -> axum::Json<LoginResult> {
    axum::Json(LoginResult {
        token: format!("token-for-{}", params.username),
    })
}

#[tokio::main]
async fn main() -> Result<(), HyErr> {
    // 只输出到控制台，级别 info：http 客户端每次请求默认打 "http call start" / "http call success" 两条 info 日志
    let _log_guard = hygiea::log::init_default()?;

    // 绑 127.0.0.1:0 让系统分配一个空闲端口，读出实际端口再给客户端用
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| err!(BaseErr::SysErr).with_source(e))?;
    let addr = listener
        .local_addr()
        .map_err(|e| err!(BaseErr::SysErr).with_source(e))?;
    let router = Router::new().route("/login", post(login));
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let client = ReqwestConfig::default().build()?;
    let params = LoginParams {
        username: "alice".to_string(),
        password: "s3cr3t".to_string(),
        device_id: "device-42".to_string(),
    };

    println!("发请求，下面几行日志里 password 会显示成 \"***\"，device_id 不会出现：\n");
    let resp = RequestConfig::with_body(Method::POST, format!("http://{addr}/login"), Json(params))
        .send::<Json<LoginResult>>(&client)
        .await?;

    println!("\n服务端返回的 token: {}", resp.body.token);

    // 请求做完了，服务端任务也不需要了；main 返回时进程退出，不用等 Ctrl+C
    server.abort();
    Ok(())
}
