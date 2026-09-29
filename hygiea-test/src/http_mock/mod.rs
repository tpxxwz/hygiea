//! 本地 mock HTTP 服务：cassette（TOML）描述接口，动态逻辑写 Rhai 脚本，一个全局 state 当"数据库"。
//! 底层是 httpmock，每个测试起自己的 server，client 的 base_url 指过来。
//!
//! # 用法
//!
//! ```ignore
//! let mock = Cassette::load(cassette_root!(), &["shop/server/login", "shop/server/items"])
//!     .db("shop/server/data/db.json")   // 可选：初始 state
//!     .start()
//!     .await?;
//! // client 的 base_url 指到 mock.base_url()，跑业务代码……
//! mock.assert_valid();   // 脚本出错、读文件失败这类 mock 自身的问题在这里报
//! ```
//!
//! # 目录
//!
//! cassette 放在调用方的 `tests/resources/httpmock/`（[`cassette_root!`](crate::cassette_root)）。
//! 怎么分目录由使用方定，比如按项目分，再分 `server/`（完整模拟服务）和 `cases/`（单接口用例）。
//! cassette 里的 `script`、`match_script`、`body_file` 和脚本里读文件的路径相对 cassette 所在的目录；
//! 脚本里 `import` 的路径相对脚本文件所在的目录。
//!
//! # state
//!
//! 一个 server 一个全局 state，所有脚本共用，改了会保留；只在内存里，测试之间互不影响。
//! 初始值只有一个来源：[`Cassette::db`] 读一个 JSON 文件（顶层必须是对象），不调时是空对象。
//! 测试中途用 [`Mocked::update_state`] 改。
//!
//! # cassette 格式
//!
//! ```toml
//! [[interactions]]
//! name = "登录"
//! request = { method = "POST", path = "/api/login", json_body_includes = { kind = "password" } }
//! script = "login.rhai"      # 或者 response = { .. } / responses = [ .. ]
//! delay_ms = 100             # 可选：先等再响应
//! match_script = "x.rhai"    # 可选：Rhai 返回 true 才算匹配
//! ```
//!
//! - 请求匹配：字段和 httpmock 的方法同名，见 `spec.rs`；`path` 可以带 `{名字}` 占位符
//! - 响应：`status`（默认 200）、`headers`、`body` / `body_file` / `json` 三选一；
//!   `json` 自动补 content-type；`body` 和文本 `body_file` 里的 `{{base}}` 换成 mock server 的地址
//! - `responses`：按调用次数依次返回，超过个数后一直返回最后一个
//! - 脚本的用法见 `script.rs`；脚本最后一个表达式就是响应，字段和 `response` 一样
//!
//! 多个 cassette 按传入顺序注册，同一个请求多条都能匹配时用先注册的。

mod error;
mod handler;
mod script;
mod spec;
mod template;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use httpmock::MockServer;
use hygiea::{HyErr, ResultExt, err};
use parking_lot::Mutex;
use rhai::Engine;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use error::HttpMockErr;
/// 原样再导出，要手写 mock 时直接用
pub use httpmock;

use handler::{Handler, Responder, Shared};
use spec::{CassetteFile, Interaction};

/// 调用方的 cassette 根目录：`<crate>/tests/resources/httpmock`。宏在调用方展开，拿到的是调用方的目录
#[macro_export]
macro_rules! cassette_root {
    () => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resources/httpmock")
    };
}

/// 要起的 mock server：加载哪些 cassette、初始 state 从哪来
pub struct Cassette {
    root: String,
    names: Vec<String>,
    db: Option<String>,
}

impl Cassette {
    /// `names` 是相对 `root` 的 cassette 路径（不带 `.toml`），按顺序注册
    pub fn load(root: impl Into<String>, names: &[&str]) -> Self {
        Self {
            root: root.into(),
            names: names.iter().map(|n| n.to_string()).collect(),
            db: None,
        }
    }

    /// 初始 state：`root` 下的 JSON 文件，顶层必须是对象，字段名就是脚本里的 `state.xxx`。再调会替换
    pub fn db(mut self, path: &str) -> Self {
        self.db = Some(path.to_string());
        self
    }

    /// 读 cassette 和 db、起 server、注册 mock
    pub async fn start(self) -> Result<Mocked, HyErr> {
        let state = match &self.db {
            Some(path) => load_db(&format!("{}/{path}", self.root))?,
            None => Value::Object(Default::default()),
        };
        let mut files = Vec::new();
        for name in &self.names {
            let path = cassette_path(&self.root, name)?;
            let file = parse_cassette(&path)?;
            files.push((path, file));
        }

        let server = MockServer::start_async().await;
        let state = rhai::serde::to_dynamic(state)
            .map_err(|e| err!(HttpMockErr::StateConvertFailed).with_source(e.to_string()))?;
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            problems: Mutex::new(Vec::new()),
            base: server.base_url(),
        });
        for (path, file) in files {
            register(&server, &shared, &path, file).await?;
        }
        Ok(Mocked { server, shared })
    }
}

fn cassette_path(root: &str, name: &str) -> Result<String, HyErr> {
    let path = format!("{root}/{name}.toml");
    if Path::new(&path).is_file() {
        Ok(path)
    } else {
        Err(err!(HttpMockErr::CassetteNotFound, path))
    }
}

fn read_file(path: &str) -> Result<String, HyErr> {
    std::fs::read_to_string(path).wrap_err(|| err!(HttpMockErr::ReadFailed, path))
}

fn parse_cassette(path: &str) -> Result<CassetteFile, HyErr> {
    toml::from_str(&read_file(path)?).wrap_err(|| err!(HttpMockErr::ParseFailed, path))
}

/// cassette 所在的目录
fn dir_of(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 读 db 文件，顶层必须是对象
fn load_db(path: &str) -> Result<Value, HyErr> {
    let value: Value = serde_json::from_str(&read_file(path)?)
        .wrap_err(|| err!(HttpMockErr::ParseFailed, path))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(err!(HttpMockErr::InvalidDb, { "path": path, "cause": "must be an object" }))
    }
}

fn compile_script(engine: &Engine, path: String) -> Result<rhai::AST, HyErr> {
    engine
        .compile_file(path.clone().into())
        .map_err(|e| err!(HttpMockErr::ScriptCompileFailed, path).with_source(e.to_string()))
}

async fn register(
    server: &MockServer,
    shared: &Arc<Shared>,
    path: &str,
    file: CassetteFile,
) -> Result<(), HyErr> {
    let dir = dir_of(path);
    let engine = Arc::new(script::build_engine(&dir));
    for it in file.interactions {
        let Interaction {
            name,
            request,
            response,
            responses,
            script,
            match_script,
            delay_ms,
        } = it;
        let invalid = |cause: &str| err!(HttpMockErr::InvalidCassette, { "path": path, "cause": format!("[{name}] {cause}") });
        let responder = match (response, responses, script) {
            (Some(one), None, None) => Responder::Sequence(vec![one]),
            (None, Some(many), None) if !many.is_empty() => Responder::Sequence(many),
            (None, None, Some(script)) => {
                Responder::Script(compile_script(&engine, format!("{dir}/{script}"))?)
            }
            _ => {
                return Err(invalid(
                    "exactly one of response / responses / script is required, responses can't be empty",
                ));
            }
        };
        let match_script = match match_script {
            Some(script) => Some(compile_script(&engine, format!("{dir}/{script}"))?),
            None => None,
        };
        let request = request.compile(path, &name)?;
        let handler = Arc::new(Handler {
            cassette: path.to_string(),
            name,
            dir: dir.clone(),
            template: request.template.clone(),
            responder,
            match_script,
            engine: engine.clone(),
            shared: shared.clone(),
            calls: AtomicUsize::new(0),
        });
        server
            .mock_async(move |when, then| {
                let mut when = request.apply(when);
                if handler.match_script.is_some() {
                    let matcher = handler.clone();
                    when = when.is_true(move |req| matcher.matches(req));
                }
                let _ = when;
                let mut then = then;
                if let Some(ms) = delay_ms {
                    then = then.delay(std::time::Duration::from_millis(ms));
                }
                then.respond_with(move |req| handler.respond(req));
            })
            .await;
    }
    Ok(())
}

/// 起好的 mock server
pub struct Mocked {
    server: MockServer,
    shared: Arc<Shared>,
}

impl Mocked {
    /// `http://127.0.0.1:<port>`
    pub fn base_url(&self) -> String {
        self.server.base_url()
    }

    pub fn url(&self, path: &str) -> String {
        self.server.url(path)
    }

    /// 底层的 httpmock server，要在 cassette 之外再手写 mock 时用
    pub fn server(&self) -> &MockServer {
        &self.server
    }

    /// 当前状态转成 `T`，断言用
    pub fn state<T: DeserializeOwned>(&self) -> Result<T, HyErr> {
        let state = self.shared.state.lock();
        rhai::serde::from_dynamic(&state)
            .map_err(|e| err!(HttpMockErr::StateConvertFailed).with_source(e.to_string()))
    }

    /// 在测试里改状态，比如清掉会话模拟过期：拿到当前状态（JSON），改完写回去
    pub fn update_state(&self, f: impl FnOnce(&mut Value)) -> Result<(), HyErr> {
        let mut state = self.shared.state.lock();
        let mut value: Value = rhai::serde::from_dynamic(&state)
            .map_err(|e| err!(HttpMockErr::StateConvertFailed).with_source(e.to_string()))?;
        f(&mut value);
        *state = rhai::serde::to_dynamic(value)
            .map_err(|e| err!(HttpMockErr::StateConvertFailed).with_source(e.to_string()))?;
        Ok(())
    }

    /// 整个替换状态
    pub fn set_state<T: Serialize>(&self, value: &T) -> Result<(), HyErr> {
        let value = rhai::serde::to_dynamic(value)
            .map_err(|e| err!(HttpMockErr::StateConvertFailed).with_source(e.to_string()))?;
        *self.shared.state.lock() = value;
        Ok(())
    }

    /// 到目前为止记下的问题：脚本出错、`body_file` 读不到、`match_script` 出错
    pub fn problems(&self) -> Vec<String> {
        self.shared.problems.lock().clone()
    }

    /// 有问题就 panic，把所有问题列出来。测试最后调
    pub fn assert_valid(&self) {
        let problems = self.problems();
        assert!(
            problems.is_empty(),
            "http_mock found {} problem(s):\n{}",
            problems.len(),
            problems
                .iter()
                .map(|p| format!("  - {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
