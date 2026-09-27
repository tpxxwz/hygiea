//! HTTP 服务的统一响应 / 错误包装（axum 组件）：`AxumHttpResponse` 序列化成 `{code, msg, data}`，
//! `AxumHttpError` 把 `HyErr` 转成响应——业务错误原样把错误码和消息给客户端，框架内置错误（项目前缀
//! `999`）对外一律显示成 "System Error"，原始信息只打在服务端日志里；另外演示了请求体大小限制。
//!
//! 本进程内起服务、发几个请求、打印结果，然后自动退出，不常驻等 Ctrl+C。
//!
//! ```bash
//! cargo run -p hygiea-examples --example http_axum_response
//! ```

use axum::Router;
use axum::routing::{get, post};
use hygiea::http::{AxumConfig, HttpLimits, arity0, arity1, build_router};
use hygiea::http_client::reqwest_client::{
    BaseHttpErr, Json, Method, RequestConfig, ReqwestConfig,
};
use hygiea::{BaseErr, HyErr, err, hy_err};
use serde::{Deserialize, Serialize};

/// 业务错误：项目前缀是本 crate 自己的 "001"（见 Cargo.toml 的 err_code_project_prefix），
/// 不是框架的 "999"，AxumHttpError 会原样把错误码和消息给客户端
#[derive(hy_err)]
#[err_code_module_prefix = "09"]
enum BizErr {
    #[error(err_code = "001", err_tpl = "User {{ name }} not found")]
    UserNotFound,
}

#[derive(Serialize, Deserialize, Debug)]
struct User {
    name: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct Echo {
    text: String,
}

/// 客户端这边解出来的响应外壳，字段和 `AxumHttpResponse` 序列化后的形状一致。
/// `FromBytes` 要求 `Json<T>` 的 `T` 同时实现 Serialize（日志预览要用）和 DeserializeOwned，
/// 这里其实只用到反序列化
#[derive(Serialize, Deserialize, Debug)]
struct Envelope<T> {
    code: String,
    msg: String,
    data: Option<T>,
}

#[tokio::main]
async fn main() -> Result<(), HyErr> {
    let _log_guard = hygiea::log::init_default()?;

    let router = Router::new()
        // 正常返回：AxumHttpResponse::ok 包一层，序列化成 {"code":SUCCESS_CODE,"msg":"","data":{...}}
        .route(
            "/user",
            get(arity0(|| async {
                Ok::<_, HyErr>(User {
                    name: "Alice".to_string(),
                })
            })),
        )
        // 业务错误：HTTP 状态码仍然是 200，客户端能看到原始的错误码和消息
        .route(
            "/missing",
            get(arity0(|| async {
                Err::<User, HyErr>(err!(BizErr::UserNotFound, "Bob"))
            })),
        )
        // 框架内置错误（core 自带的 BaseErr，项目前缀 999）：对外只显示 "System Error"，
        // err_tpl 里的参数（可能带内部信息）不会出现在响应里，只会打在服务端日志
        .route(
            "/boom",
            get(arity0(|| async {
                Err::<User, HyErr>(err!(BaseErr::RegexError, "internal-only-pattern"))
            })),
        )
        // 一元 handler：原样回显
        .route(
            "/echo",
            post(arity1(
                |input: Echo| async move { Ok::<Echo, HyErr>(input) },
            )),
        );
    // 用框架的 build_router 按配置加中间件：请求体超过 16 字节就在业务代码之前被拒绝（413）。
    // AxumComponent 启动时也是用它拼出最终的 router
    let config = AxumConfig {
        limits: Some(HttpLimits {
            max_body_size: Some(16),
            ..Default::default()
        }),
        ..Default::default()
    };
    let router = build_router(router, &config);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| err!(BaseErr::SysErr).with_source(e))?;
    let addr = listener
        .local_addr()
        .map_err(|e| err!(BaseErr::SysErr).with_source(e))?;
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let client = ReqwestConfig::default().build()?;
    let url = |path: &str| format!("http://{addr}{path}");

    // 1. 正常返回
    let resp = RequestConfig::plain(Method::GET, url("/user"))
        .send::<Json<Envelope<User>>>(&client)
        .await?;
    let body = resp.body.0;
    println!(
        "1. GET /user          -> code={:?} data={:?}",
        body.code, body.data
    );

    // 2. 业务错误：HTTP 状态码仍是 200，body 里是原始的错误码和消息
    let resp = RequestConfig::plain(Method::GET, url("/missing"))
        .send::<Json<Envelope<User>>>(&client)
        .await?;
    let body = resp.body.0;
    println!(
        "2. GET /missing       -> code={:?} msg={:?}",
        body.code, body.msg
    );

    // 3. 框架内置错误：对外只有 "System Error"，看不到 "internal-only-pattern"
    let resp = RequestConfig::plain(Method::GET, url("/boom"))
        .send::<Json<Envelope<User>>>(&client)
        .await?;
    let body = resp.body.0;
    println!(
        "3. GET /boom          -> code={:?} msg={:?}",
        body.code, body.msg
    );

    // 4. 正常大小的请求体
    let resp = RequestConfig::with_body(
        Method::POST,
        url("/echo"),
        Json(Echo {
            text: "hi".to_string(),
        }),
    )
    .send::<Json<Envelope<Echo>>>(&client)
    .await?;
    let body = resp.body.0;
    println!(
        "4. POST /echo         -> code={:?} data={:?}",
        body.code, body.data
    );

    // 5. 请求体超过限制：被 build_router 加的 body 大小限制挡在业务代码之前，返回 413，
    //    客户端这边报 BaseHttpErr::NonSuccessStatus
    let big = Echo {
        text: "x".repeat(64),
    };
    match RequestConfig::with_body(Method::POST, url("/echo"), Json(big))
        .send::<Json<Envelope<Echo>>>(&client)
        .await
    {
        Ok(_) => println!("5. POST /echo（超限）-> 意外地成功了"),
        Err(e) => println!(
            "5. POST /echo（超限）-> is(NonSuccessStatus)={}, status={}",
            e.is(BaseHttpErr::NonSuccessStatus),
            e.err_args()["status"]
        ),
    }

    server.abort();
    Ok(())
}
