//! 本地 mock HTTP 服务：一个目录就是一个模拟服务，cassette（TOML）声明路由，动态逻辑写 Rhai 脚本，
//! 一个全局 state 当"数据库"。底层是 httpmock，每个测试起自己的 server，client 的 base_url 指过来。
//!
//! # 定位：模拟一个带状态的普通服务器
//!
//! 它要做的是"一个真的服务器"，不是传统的按调用打桩：
//!
//! - 一个服务只有一套逻辑，所有测试挂的都是同一个服务目录；响应由 **state（相当于数据库）和请求内容** 决定，
//!   和真服务器一样：登录写会话、上传写文件、会话被清掉就返回过期
//! - 特殊数据、异常场景在测试里改 state（[`Mocked::update_state`]）或换文件（[`Mocked::set_file`]）造出来，
//!   不为每个场景另写一套规则
//! - 验证看结果：client 拿到的返回值 / 错误，和服务端 state 有没有发生预期的变化。
//!   **不要断言某个接口被调了几次**：那是 client 的实现细节，也不是一个真服务器能对外表现出来的东西
//!
//! 下面几个功能是为少数场景留的口子，别拿来组织常规逻辑：
//!
//! - handler 链（`handlers = [..]`）：少用。服务的逻辑写在一个脚本里，共用的部分放 `common.rhai` 用 `import`
//! - 脚本里的 `calls`（这条路由第几次被调用）和 `nth(calls, [..])`：只给"同样的请求、服务端也没有状态变化，
//!   结果却不一样"这种情况用，比如测 client 对上游偶发 503、超时的处理。其他情况一律由 state 决定
//! - `delay_ms`：测超时用，阻塞等待
//!
//! 和真实服务是否一致不归它管，mock 的响应和数据都是自己写的；由连真实服务的 live 测试保证。
//!
//! # 用法
//!
//! ```ignore
//! let mock = Cassette::load(cassette_root!(), "shop").start().await?;
//! // client 的 base_url 指到 mock.base_url()，跑业务代码……
//! mock.assert_valid();   // 可选：脚本出错、没有路由接住的请求这类问题在这里报；不调的话 drop 时自动检查
//! ```
//!
//! 也可以全用代码定义（[`Cassette::new`]），或者在目录的基础上用代码补充：
//!
//! ```ignore
//! Cassette::load(cassette_root!(), "shop")
//!     .state(json!({ .. }))                                   // 整个替换 data/state.json
//!     .file("hello.txt", "hi")                                // 覆盖 / 新增文件表里的文件
//!     .route("GET", "/api/extra", Handler::response(Response::json(json!({ "ok": true }))))
//!     .start()
//!     .await?;
//! ```
//!
//! 代码加的路由和目录里的一起检查冲突，行为完全一样（见 [`Handler`]、[`Response`]）。
//!
//! # 目录
//!
//! cassette 放在调用方的 `tests/resources/httpmock/`（[`cassette_root!`](crate::cassette_root)），
//! 一个服务一个目录，启动时全部加载：
//!
//! ```text
//! shop/
//! ├── routes.toml     路由：目录下所有 toml 都挂上（不含子目录），一般一个 routes.toml 就够
//! ├── rhai/           脚本，handler 的 `script`、request 的 `match_script` 相对这个目录，下面怎么分子目录都行
//! └── data/
//!     ├── state.json     state 的初始值，顶层是对象；没有就是空对象
//!     └── static/     启动时读进内存文件表
//! ```
//!
//! 脚本里 `import` 的路径相对脚本文件所在的目录。
//!
//! # 路由
//!
//! 由 method + `path` 决定，`path` 可以带 `{名字}` / `{名字:正则}` 变量（见 `template.rs`）。
//! 启动时检查：同一个 method 下两条路由能匹配同一个路径就报 [`HttpMockErr::RouteConflict`]，
//! 字面量路径（不带变量）除外，它比模板优先，`/decks/folders` 和 `/decks/{deck}` 可以同时有。
//! 所以文件的加载顺序无所谓。其他匹配条件（query、header、body……）是额外要求，对不上和没有路由一样：
//! 记一条问题，返回 404。
//!
//! # 文件
//!
//! `data/static/` 下的文件启动时读进内存，一个 server 一份，key 是相对 `data/static/` 的路径。
//! `body_file`、`read_json`、`read_text` 读这份表，脚本的 `write_file` 也只写进这份表（比如存 client
//! 上传的文件，之后 `body_file` 就能返回它），不落盘，测试之间互不影响。
//! 测试里用 [`Mocked::file`] 查、[`Mocked::set_file`] 换。
//!
//! # state
//!
//! 一个 server 一个全局 state，所有脚本共用，改了会保留；只在内存里，测试之间互不影响。
//! 初始值只有 `data/state.json` 一个来源，测试中途用 [`Mocked::update_state`] 改。
//! 脚本可以往 state 里放 `DateTime`，转 JSON 时是 UTC 的 RFC 3339 字符串，见 `script.rs`。
//!
//! # cassette 格式
//!
//! ```toml
//! [[interactions]]
//! name = "登录"
//! request = { method = "POST", path = "/api/login" }   # 可选 match_script = "x.rhai"
//! handler = { script = "login.rhai" }                    # 或 response = { .. }；可选 delay_ms
//!
//! [[interactions]]
//! name = "获取牌组"
//! request = { method = "GET", path = "/decks/{deck}" }
//! handlers = [
//!     { script = "auth.rhai" },                          # 返回 () 交给下一个
//!     { delay_ms = 300, script = "get_deck.rhai" },
//! ]
//! ```
//!
//! - 请求匹配：`method`、`path` 必填，其他字段和 httpmock 的方法同名，见 `spec.rs`
//! - `handler` / `handlers` 二选一，`handlers` 至少一个；每个 handler 里 `script` / `response` 二选一，
//!   `delay_ms` 可选（毫秒，阻塞等待：等的时候这个 server 处理不了别的请求）
//! - 链按顺序执行：脚本返回 `()` 交给下一个，第一个给出响应的生效；整条链都没给出响应记一条问题、返回 500。
//!   链是少数场景用的（见上面的"定位"），常规写 `handler = { .. }` 一个就够
//! - 响应：`status`（默认 200）、`headers`、`body` / `body_file` / `json` 三选一；
//!   `json` 自动补 content-type；`body` 和文本 `body_file` 里的 `{{base}}` 换成 mock server 的地址
//! - 脚本的用法见 `script.rs`；脚本最后一个表达式就是响应，字段和 `response` 一样
//! - 脚本启动时编译，严格变量模式：语法错误、用了没定义的变量（`state` 拼成 `stat`）启动就报
//!   `ScriptCompileFailed`；字段名、方法名写错、数据相关的错误只能跑到那一行才知道（`ScriptFailed`）。
//!   一次最多执行一百万步，死循环会被中断
//!
//! # 出了问题
//!
//! - 还没起来就错了（目录不存在、toml / 脚本写错、路由冲突、固定响应的 `body_file` 不存在……）：
//!   `start()` 返回 [`HttpMockErr`]
//! - 起来以后处理请求时出的问题（没有路由、脚本出错、整条链没给出响应……）：是 [`HttpMockRuntimeErr`]，
//!   记进 [`Mocked::problems`]，同时作为这次请求的响应返回（没有路由 404，其他 500，body 是
//!   `{"err_code":..,"message":..}`，响应头 [`PROBLEM_HEADER`] 是错误码，和模拟出来的服务端错误区分开）。
//!   问题一直累积到 mock 结束：测试最后调 [`Mocked::assert_valid`] 统一报，不调的话 mock 被 drop 时自动检查，
//!   还有问题就 panic（测试已经在 panic 时不查）。故意制造问题的测试用 [`Mocked::take_problems`] 取走自己断言；
//!   按类型判断用 [`Mocked::has_problem`]
//! - 没有路由时消息里写了原因：路由对上了但额外条件没对上 / 路径只有别的 method / 没有路径对得上

mod builder;
mod error;
mod handler;
mod route;
mod script;
mod spec;
mod template;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use httpmock::MockServer;
use httpmock::prelude::HttpMockRequest;
use hygiea::{HyErr, ResultExt, err};
use parking_lot::Mutex;
use rhai::Engine;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use builder::{Handler, Response};
pub use error::{HttpMockErr, HttpMockRuntimeErr};
pub use handler::PROBLEM_HEADER;

use handler::{RouteHandler, Shared, Stage, Step};
use spec::{Action, CassetteFile, CompiledRequest, HandlerDef, RequestSpec, RouteDef};

/// 调用方的 cassette 根目录：`<crate>/tests/resources/httpmock`。宏在调用方展开，拿到的是调用方的目录
#[macro_export]
macro_rules! cassette_root {
    () => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resources/httpmock")
    };
}

const STATE_FILE: &str = "data/state.json";
const STATIC_DIR: &str = "data/static";
const RHAI_DIR: &str = "rhai";
/// 代码定义的路由、state、文件在报错里的来源
const CODE: &str = "(code)";

/// 要起的 mock server：一个服务目录（[`load`](Self::load)），或者全用代码定义（[`new`](Self::new)），
/// 也可以在目录的基础上用代码补充
#[derive(Default)]
pub struct Cassette {
    dir: Option<String>,
    state: Option<Value>,
    files: Vec<(String, Vec<u8>)>,
    routes: Vec<RouteDef>,
}

impl Cassette {
    /// 空服务，路由、state、文件都用代码加
    pub fn new() -> Self {
        Self::default()
    }

    /// `dir` 是相对 `root` 的服务目录，比如 `moji`、`momo/markji`
    pub fn load(root: &str, dir: &str) -> Self {
        Self {
            dir: Some(format!("{root}/{dir}")),
            ..Self::default()
        }
    }

    /// state 的初始值，整个替换 `data/state.json`；顶层必须是对象，不是的话 `start()` 报错
    pub fn state(mut self, state: Value) -> Self {
        self.state = Some(state);
        self
    }

    /// 文件表里加一个文件（和 `data/static/` 下的同名就覆盖），`path` 相对 `data/static/`
    pub fn file(mut self, path: &str, content: impl Into<Vec<u8>>) -> Self {
        self.files.push((path.to_string(), content.into()));
        self
    }

    /// 加一条路由，一个 handler。`path` 的写法和 toml 一样（`{名字}` / `{名字:正则}`）
    pub fn route(self, method: &str, path: &str, handler: Handler) -> Self {
        self.route_chain(method, path, [handler])
    }

    /// 加一条路由，handler 链（至少一个）
    pub fn route_chain(
        mut self,
        method: &str,
        path: &str,
        handlers: impl IntoIterator<Item = Handler>,
    ) -> Self {
        self.routes.push(RouteDef {
            name: format!("{method} {path}"),
            request: RequestSpec::new(method, path),
            handlers: handlers.into_iter().map(|h| h.0).collect(),
        });
        self
    }

    /// 读目录（有的话），合上代码加的 state、文件、路由，检查路由冲突，起 server、注册路由
    pub async fn start(self) -> Result<Mocked, HyErr> {
        let dir = self.dir;
        let (mut state, mut files, cassettes) = match &dir {
            Some(dir) => load_dir(dir)?,
            None => (
                Value::Object(Default::default()),
                HashMap::new(),
                Vec::new(),
            ),
        };
        if let Some(code_state) = self.state {
            if !code_state.is_object() {
                return Err(
                    err!(HttpMockErr::InvalidState, { "path": CODE, "cause": "must be an object" }),
                );
            }
            state = code_state;
        }
        for (path, content) in self.files {
            let key = file_key(&path).map_err(
                |cause| err!(HttpMockErr::InvalidCassette, { "path": CODE, "cause": cause }),
            )?;
            files.insert(key, content);
        }

        let server = MockServer::start_async().await;
        let state = rhai::serde::to_dynamic(state)
            .map_err(|e| err!(HttpMockErr::StateConvertFailed).with_source(e.to_string()))?;
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            problems: Mutex::new(Vec::new()),
            files: Mutex::new(files),
            base: server.base_url(),
        });
        let engine = Arc::new(script::build_engine(shared.clone()));
        let mut routes = Vec::new();
        for (path, file) in cassettes {
            for it in file.interactions {
                let def = it.into_def().map_err(
                    |cause| err!(HttpMockErr::InvalidCassette, { "path": &path, "cause": cause }),
                )?;
                routes.push(compile_route(&engine, &shared, dir.as_deref(), &path, def)?);
            }
        }
        for it in self.routes {
            routes.push(compile_route(&engine, &shared, dir.as_deref(), CODE, it)?);
        }
        check_conflicts(&routes)?;
        // 字面量路径先注册：httpmock 用先注册的，模板和字面量都能匹配时字面量优先
        routes.sort_by_key(|route| !route.request.template.literal);
        let summaries = routes
            .iter()
            .map(|route| RouteSummary {
                method: route.request.method.clone(),
                template: route.request.template.clone(),
                label: format!("{} ({})", route.handler.name, route.handler.cassette),
            })
            .collect();
        for route in routes {
            register(&server, route).await;
        }
        register_fallback(&server, &shared, summaries).await;
        Ok(Mocked { server, shared })
    }
}

type Loaded = (Value, HashMap<String, Vec<u8>>, Vec<(String, CassetteFile)>);

/// 服务目录：state、文件表、所有 cassette
fn load_dir(dir: &str) -> Result<Loaded, HyErr> {
    if !Path::new(dir).is_dir() {
        return Err(err!(HttpMockErr::CassetteNotFound, dir));
    }
    let state_file = format!("{dir}/{STATE_FILE}");
    let state = if Path::new(&state_file).is_file() {
        load_state(&state_file)?
    } else {
        Value::Object(Default::default())
    };
    let static_dir = format!("{dir}/{STATIC_DIR}");
    let files = if Path::new(&static_dir).is_dir() {
        load_files(&static_dir)?
    } else {
        HashMap::new()
    };
    let mut cassettes = Vec::new();
    for path in toml_files(dir)? {
        let file = parse_cassette(&path)?;
        cassettes.push((path, file));
    }
    Ok((state, files, cassettes))
}

/// 目录下的所有 `.toml`（不含子目录），按文件名排序，报错顺序稳定
fn toml_files(dir: &str) -> Result<Vec<String>, HyErr> {
    let read_failed = || err!(HttpMockErr::ReadFailed, dir);
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).wrap_err(read_failed)? {
        let path = entry.wrap_err(read_failed)?.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "toml") {
            paths.push(path.to_string_lossy().into_owned());
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_file(path: &str) -> Result<String, HyErr> {
    std::fs::read_to_string(path).wrap_err(|| err!(HttpMockErr::ReadFailed, path))
}

fn parse_cassette(path: &str) -> Result<CassetteFile, HyErr> {
    toml::from_str(&read_file(path)?).wrap_err(|| err!(HttpMockErr::ParseFailed, path))
}

/// `dir` 下的所有文件读进内存，key 是相对 `dir` 的路径，用 `/` 分隔
fn load_files(dir: &str) -> Result<HashMap<String, Vec<u8>>, HyErr> {
    let read_failed = |path: &Path| err!(HttpMockErr::ReadFailed, path.display().to_string());
    let mut files = HashMap::new();
    let mut dirs = vec![PathBuf::from(dir)];
    while let Some(current) = dirs.pop() {
        for entry in std::fs::read_dir(&current).wrap_err(|| read_failed(&current))? {
            let path = entry.wrap_err(|| read_failed(&current))?.path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).wrap_err(|| read_failed(&path))?;
            let key = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.insert(key, bytes);
        }
    }
    Ok(files)
}

/// 文件表的 key：处理 `.` 和 `..`，不能跳出 `data/static/`
pub(crate) fn file_key(path: &str) -> Result<String, String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!("path escapes {STATIC_DIR}: {path}"));
                }
            }
            part => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

/// 读 state 文件，顶层必须是对象
fn load_state(path: &str) -> Result<Value, HyErr> {
    let value: Value = serde_json::from_str(&read_file(path)?)
        .wrap_err(|| err!(HttpMockErr::ParseFailed, path))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(err!(HttpMockErr::InvalidState, { "path": path, "cause": "must be an object" }))
    }
}

/// `vars` 是脚本能用的外部变量（[`script::HANDLER_VARS`] / [`script::MATCH_VARS`]），严格变量模式下编译要用
fn compile_script(engine: &Engine, path: String, vars: &[&str]) -> Result<rhai::AST, HyErr> {
    engine
        .compile_file_with_scope(&script::compile_scope(vars), path.clone().into())
        .map_err(|e| err!(HttpMockErr::ScriptCompileFailed, path).with_source(e.to_string()))
}

/// 一条编译好、还没注册的路由
struct Route {
    request: CompiledRequest,
    handler: Arc<RouteHandler>,
}

impl Route {
    /// 报错时用：`GET /decks/{deck} (markji/get_deck.toml [获取牌组信息])`
    fn label(&self) -> String {
        format!(
            "{} {} ({} [{}])",
            self.request.method,
            self.request.path(),
            self.handler.cassette,
            self.handler.name
        )
    }
}

/// `dir` 是服务目录（纯代码定义时没有），`path` 是 cassette 文件（代码定义的是 [`CODE`]）
fn compile_route(
    engine: &Arc<Engine>,
    shared: &Arc<Shared>,
    dir: Option<&str>,
    path: &str,
    def: RouteDef,
) -> Result<Route, HyErr> {
    let RouteDef {
        name,
        mut request,
        handlers,
    } = def;
    let invalid = |cause: &str| err!(HttpMockErr::InvalidCassette, { "path": path, "cause": format!("[{name}] {cause}") });
    let cassette = dir
        .and_then(|dir| Path::new(path).strip_prefix(dir).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    if handlers.is_empty() {
        return Err(invalid("handlers needs at least one handler"));
    }
    let mut chain = Vec::with_capacity(handlers.len());
    for (i, HandlerDef { delay_ms, action }) in handlers.into_iter().enumerate() {
        let desc = match &action {
            Action::Script(script) => format!("script {script}"),
            Action::InlineScript(_) => "inline script".to_string(),
            Action::Response(_) => "response".to_string(),
        };
        let step = match action {
            Action::Script(script) => Step::Script(compile_script(
                engine,
                script_path(dir, &script).map_err(|c| invalid(&c))?,
                &script::HANDLER_VARS,
            )?),
            Action::InlineScript(src) => Step::Script(
                engine
                    .compile_with_scope(&script::compile_scope(&script::HANDLER_VARS), &src)
                    .map_err(|e| {
                        err!(HttpMockErr::ScriptCompileFailed, format!("{path} [{name}]"))
                            .with_source(e.to_string())
                    })?,
            ),
            Action::Response(response) => {
                // 固定响应是写死的，能查的启动时就查
                let bodies = [
                    response.body.is_some(),
                    response.body_file.is_some(),
                    response.json.is_some(),
                ];
                if bodies.iter().filter(|b| **b).count() > 1 {
                    return Err(invalid(&format!(
                        "handler #{}: only one of body / body_file / json is allowed",
                        i + 1
                    )));
                }
                // body_file 查文件表（data/static 已经读进内存了）
                if let Some(file) = &response.body_file {
                    let exists =
                        file_key(file).is_ok_and(|key| shared.files.lock().contains_key(&key));
                    if !exists {
                        return Err(err!(HttpMockErr::FileNotFound, {
                            "route": format!("{cassette} [{name}] handler #{}", i + 1),
                            "file": file,
                        }));
                    }
                }
                Step::Fixed(response)
            }
        };
        chain.push(Stage {
            delay_ms,
            step,
            desc,
        });
    }
    let match_script = match request.match_script.take() {
        Some(script) => Some(compile_script(
            engine,
            script_path(dir, &script).map_err(|c| invalid(&c))?,
            &script::MATCH_VARS,
        )?),
        None => None,
    };
    let request = request.compile(path, &name)?;
    let handler = Arc::new(RouteHandler {
        cassette,
        name,
        template: request.template.clone(),
        chain,
        match_script,
        engine: engine.clone(),
        shared: shared.clone(),
        calls: AtomicUsize::new(0),
    });
    Ok(Route { request, handler })
}

/// 脚本文件的完整路径；纯代码定义的服务没有 `rhai/`，只能用行内脚本
fn script_path(dir: Option<&str>, script: &str) -> Result<String, String> {
    match dir {
        Some(dir) => Ok(format!("{dir}/{RHAI_DIR}/{script}")),
        None => Err(format!(
            "script file `{script}` needs a service dir (Cassette::load), use Handler::script_str instead"
        )),
    }
}

/// 同一个 method 下两条路由能匹配同一个路径就报错；一条是字面量、一条是模板的不算，字面量优先
fn check_conflicts(routes: &[Route]) -> Result<(), HyErr> {
    for (i, a) in routes.iter().enumerate() {
        for b in &routes[i + 1..] {
            if a.request.method != b.request.method {
                continue;
            }
            let (ta, tb) = (&a.request.template, &b.request.template);
            let example = match (ta.literal, tb.literal) {
                (true, true) => {
                    (a.request.path() == b.request.path()).then(|| a.request.path().to_string())
                }
                (true, false) | (false, true) => None,
                (false, false) => route::intersection(ta.regex.as_str(), tb.regex.as_str())
                    .map_err(|cause| {
                        err!(HttpMockErr::InvalidTemplate, a.request.path()).with_source(cause)
                    })?,
            };
            if let Some(example) = example {
                return Err(err!(HttpMockErr::RouteConflict, {
                    "a": a.label(),
                    "b": b.label(),
                    "example": example,
                }));
            }
        }
    }
    Ok(())
}

async fn register(server: &MockServer, route: Route) {
    let Route { request, handler } = route;
    server
        .mock_async(move |when, then| {
            let mut when = request.apply(when);
            if handler.match_script.is_some() {
                let matcher = handler.clone();
                when = when.is_true(move |req| matcher.matches(req));
            }
            let _ = when;
            then.respond_with(move |req| handler.respond(req));
        })
        .await;
}

/// 最后注册、什么都匹配：前面的路由都没接住的请求记一条问题，返回 404
/// 兜底时说明为什么没接住用的：每条路由的 method、路径模板、名字
struct RouteSummary {
    method: http::Method,
    template: template::PathTemplate,
    label: String,
}

/// 没接住的原因：路径对上了但额外条件没对上 / 路径只有别的 method / 没有路由的路径对得上
fn no_route_reason(summaries: &[RouteSummary], method: &str, path: &str) -> String {
    let path_matched: Vec<&RouteSummary> = summaries
        .iter()
        .filter(|s| s.template.regex.is_match(path))
        .collect();
    let same_method: Vec<&str> = path_matched
        .iter()
        .filter(|s| s.method.as_str() == method)
        .map(|s| s.label.as_str())
        .collect();
    if !same_method.is_empty() {
        return format!(
            "method and path match {}, but its other conditions (query / header / cookie / body / match_script) don't",
            same_method.join(", ")
        );
    }
    let mut others: Vec<&str> = path_matched.iter().map(|s| s.method.as_str()).collect();
    others.sort_unstable();
    others.dedup();
    if others.is_empty() {
        format!("no route's path matches {path}")
    } else {
        format!("path {path} only has routes for {}", others.join(", "))
    }
}

async fn register_fallback(
    server: &MockServer,
    shared: &Arc<Shared>,
    summaries: Vec<RouteSummary>,
) {
    let shared = shared.clone();
    server
        .mock_async(move |_when, then| {
            then.respond_with(move |req: &HttpMockRequest| {
                let uri = req.uri();
                let target = uri.path_and_query().map_or(uri.path(), |pq| pq.as_str());
                let reason = no_route_reason(&summaries, req.method_str(), uri.path());
                shared.fail(
                    404,
                    err!(HttpMockRuntimeErr::NoRoute, {
                        "request": format!("{} {target}", req.method_str()),
                        "reason": reason,
                    }),
                )
            });
        })
        .await;
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

    /// 当前状态转成 `T`，断言用
    pub fn state<T: DeserializeOwned>(&self) -> Result<T, HyErr> {
        let state = script::to_json(&self.shared.state.lock())
            .map_err(|e| err!(HttpMockErr::StateConvertFailed).with_source(e))?;
        serde_json::from_value(state)
            .map_err(|e| err!(HttpMockErr::StateConvertFailed).with_source(e.to_string()))
    }

    /// 在测试里改状态，比如清掉会话模拟过期：拿到当前状态（JSON），改完写回去
    pub fn update_state(&self, f: impl FnOnce(&mut Value)) -> Result<(), HyErr> {
        let mut state = self.shared.state.lock();
        let mut value = script::to_json(&state)
            .map_err(|e| err!(HttpMockErr::StateConvertFailed).with_source(e))?;
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

    /// 内存文件表里的文件，`path` 相对 `data/static/`：启动时读进来的，或者脚本 `write_file` 写的
    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        self.shared.files.lock().get(path).cloned()
    }

    /// 在测试里换掉文件表里的文件，比如让首页返回异常内容；只影响这个 server
    pub fn set_file(&self, path: &str, content: impl Into<Vec<u8>>) {
        self.shared
            .files
            .lock()
            .insert(path.to_string(), content.into());
    }

    /// 到目前为止记下的问题（[`HttpMockRuntimeErr`]），每条是 `{:#}`：消息加上原因链
    pub fn problems(&self) -> Vec<String> {
        self.shared
            .problems
            .lock()
            .iter()
            .map(|e| format!("{e:#}"))
            .collect()
    }

    /// 有没有某种问题：`mock.has_problem(HttpMockRuntimeErr::NoRoute)`
    pub fn has_problem(&self, kind: impl hygiea::ErrKind) -> bool {
        let code = kind.err_code();
        self.shared
            .problems
            .lock()
            .iter()
            .any(|e| e.err_code() == code)
    }

    /// 取走到目前为止的问题（同 [`problems`](Self::problems) 的格式），取完清空。
    /// 故意制造问题的测试用：取出来自己断言，drop 时就不会再报
    pub fn take_problems(&self) -> Vec<String> {
        std::mem::take(&mut *self.shared.problems.lock())
            .iter()
            .map(|e| format!("{e:#}"))
            .collect()
    }

    /// 有问题就 panic，把所有问题列出来。不清空；不调也行，drop 时会自动检查
    pub fn assert_valid(&self) {
        let problems = self.problems();
        assert!(problems.is_empty(), "{}", report(&problems));
    }
}

fn report(problems: &[String]) -> String {
    format!(
        "http_mock found {} problem(s):\n{}",
        problems.len(),
        problems
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// 测试结束 mock 被 drop 时还有问题就 panic，免得忘了 [`Mocked::assert_valid`] 把问题吞掉。
/// 测试已经在 panic（断言失败、`assert_valid` 报了）时不再检查，不叠第二个 panic；
/// 故意制造问题的测试用 [`Mocked::take_problems`] 取走
impl Drop for Mocked {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        let problems = self.problems();
        if !problems.is_empty() {
            panic!(
                "{}\n(not checked before the mock was dropped; use take_problems() if they are expected)",
                report(&problems)
            );
        }
    }
}
