//! cassette 加载：[`start`] 按 `tests/resources/httpmock/<name>` 找文件，支持两种格式，
//! 同一个名字只能有一种（两种都有就报错）。[`start_all`] 把多个 cassette 挂到同一个 server，
//! 按传入顺序注册，先注册的优先。以后挪进 hygiea-test 的 `Cassette`。
//!
//! - `<name>.yaml` / `<name>.yml`：httpmock 原生的 YAML 格式，原样交给 `playback_from_yaml`，
//!   不做任何处理（没有 `body_file`、`{{base}}`、脚本和状态）。httpmock 录制出来的文件可以直接用
//! - `<name>.toml`：自定义格式，用 httpmock 的代码 API 注册成 mock，下面说明
//!
//! 目录按项目分，每个项目下 `server/`（完整模拟服务）、`cases/`（单接口用例）；cassette 里的
//! `script`、`state_file`、`body_file` 和脚本里 `read_json` / `read_text` 的路径都相对 cassette 所在的目录。
//!
//! TOML 格式里一条 interaction 的响应三选一：
//! - `response`：固定响应
//! - `responses`：按调用顺序依次返回，调用次数超过个数后一直返回最后一个
//! - `script`：Rhai 脚本。脚本里能用的变量：
//!   - `request`：请求，query / header / body 都用它的方法取，见 [`Req`]
//!   - `read_json(path)` / `read_text(path)`：读数据文件（比如词库），模拟服务端的数据
//!   - `import "common" as c;`：引用同目录下的 `common.rhai`，放几个接口共用的函数
//!   - `calls`：这条 interaction 第几次被调用，从 1 开始
//!   - `base`：当前 mock server 的地址，拼返回给客户端的 URL 用
//!   - `state`：同一个 server 上所有脚本共用的状态，初始值是各 cassette 的 `[state]` 和 `state_file`
//!     合并的结果，改了会保留。一个 server 一份，不同 server 之间互不影响
//!
//!   脚本最后一个表达式返回响应，字段和 `response` 一样：`#{ status: 200, json: #{ ... } }`
//!
//! 响应字段：`status`（默认 200）、`headers`、`body` / `body_file` / `json` 三选一。
//! `body`、文本的 `body_file` 里的 `{{base}}` 换成当前 mock server 的地址。
//! 状态只在内存里，测试结束就丢；测试里用 [`Mocked::state`] 读出来断言

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use httpmock::prelude::*;
use rhai::{AST, Dynamic, Engine, Scope};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resources/httpmock");

/// cassette 文件所在的目录，cassette 和脚本里的相对路径都相对它
fn dir_of(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CassetteFile {
    /// 初始状态，写在 cassette 里
    #[serde(default)]
    state: serde_json::Map<String, Value>,
    /// 初始状态，从 JSON / TOML 文件读（路径相对 cassette 的目录），顶层必须是对象；和 `[state]` 的键合并
    state_file: Option<String>,
    #[serde(default)]
    interactions: Vec<Interaction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Interaction {
    name: String,
    request: RequestSpec,
    response: Option<ResponseSpec>,
    responses: Option<Vec<ResponseSpec>>,
    script: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestSpec {
    method: String,
    /// 路径完全相等；和 `path_prefix` 至少写一个。可以写 `{名字}` 占位符匹配一段路径
    /// （不含 `/`），比如 `/tts/{voice}/{id}.mp3`，脚本里用 `request.param("id")` 取
    path: Option<String>,
    /// 路径前缀，比如 `/tts/` 匹配所有下载地址
    path_prefix: Option<String>,
    #[serde(default)]
    query: BTreeMap<String, String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    json_body: Option<Value>,
    json_body_includes: Option<Value>,
    #[serde(default)]
    body_includes: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseSpec {
    #[serde(default = "default_status")]
    status: u16,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    body: Option<String>,
    body_file: Option<String>,
    json: Option<Value>,
}

fn default_status() -> u16 {
    200
}

/// 响应怎么来：固定序列（固定响应就是只有一个的序列），或者 Rhai 脚本
enum Responder {
    Sequence(Vec<ResponseSpec>),
    Script(AST),
}

/// 起好的 mock server，和它的运行时状态
pub struct Mocked {
    pub server: MockServer,
    state: Arc<Mutex<Dynamic>>,
}

impl Mocked {
    pub fn url(&self, path: &str) -> String {
        self.server.url(path)
    }

    pub fn base_url(&self) -> String {
        self.server.base_url()
    }

    /// 当前状态转成结构体，测试里断言用
    pub fn state<T: DeserializeOwned>(&self) -> T {
        let state = self.state.lock().unwrap();
        rhai::serde::from_dynamic(&state).unwrap()
    }

    /// 在测试里改状态，比如清掉会话模拟过期。拿到当前状态（JSON），改完的值写回去
    pub fn update_state(&self, f: impl FnOnce(&mut Value)) {
        let mut state = self.state.lock().unwrap();
        let mut value: Value = rhai::serde::from_dynamic(&state).unwrap();
        f(&mut value);
        *state = rhai::serde::to_dynamic(value).unwrap();
    }
}

/// 起一个 mock server，加载 `<name>` 对应的 cassette
pub async fn start(name: &str) -> Mocked {
    start_all(&[name]).await
}

/// 多个 cassette 挂到同一个 mock server 上。httpmock 按注册顺序匹配、先注册的优先，
/// 所以 `names` 的顺序就是优先级：同一个请求多个 cassette 都能匹配时，用排在前面的。
/// 各 TOML 的 `[state]`、`state_file` 合并成一份共用，同一个键出现两次就报错
pub async fn start_all(names: &[&str]) -> Mocked {
    let cassettes: Vec<(String, String)> = names
        .iter()
        .map(|name| {
            let path = find(name);
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            (path, text)
        })
        .collect();

    // 先把所有 TOML 解析出来、合并状态，脚本拿到的是同一份
    let mut merged = serde_json::Map::new();
    let mut parsed = Vec::new();
    for (path, text) in &cassettes {
        if path.ends_with(".toml") {
            let file: CassetteFile =
                toml::from_str(text).unwrap_or_else(|e| panic!("parse {path}: {e}"));
            let from_file = file
                .state_file
                .as_deref()
                .map(|state_file| load_state_file(&format!("{}/{state_file}", dir_of(path))));
            for (key, value) in from_file.into_iter().flatten().chain(file.state.clone()) {
                if merged.insert(key.clone(), value).is_some() {
                    panic!("state key `{key}` defined more than once (again in {path})");
                }
            }
            parsed.push(Some(file));
        } else {
            parsed.push(None);
        }
    }

    let server = MockServer::start_async().await;
    let state = Arc::new(Mutex::new(rhai::serde::to_dynamic(&merged).unwrap()));
    for ((path, text), file) in cassettes.iter().zip(parsed) {
        match file {
            Some(file) => register_toml(&server, &state, path, file).await,
            // 原生 YAML：httpmock 自己解析，不参与状态
            None => {
                server.playback_from_yaml_async(text).await;
            }
        }
    }
    Mocked { server, state }
}

/// `path` 里有 `{名字}` 占位符就转成正则：占位符匹配一段不含 `/` 的路径，其他字符原样匹配
fn path_template(path: &str) -> Option<regex::Regex> {
    if !path.contains('{') {
        return None;
    }
    let placeholder = regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
    let mut pattern = String::from("^");
    let mut last = 0;
    for caps in placeholder.captures_iter(path) {
        let whole = caps.get(0).unwrap();
        pattern.push_str(&regex::escape(&path[last..whole.start()]));
        pattern.push_str(&format!("(?P<{}>[^/]+)", &caps[1]));
        last = whole.end();
    }
    pattern.push_str(&regex::escape(&path[last..]));
    pattern.push('$');
    Some(regex::Regex::new(&pattern).unwrap_or_else(|e| panic!("path template {path}: {e}")))
}

/// 读 state 初始数据，按扩展名用 JSON 或 TOML 解析；`full` 是完整路径
fn load_state_file(full: &str) -> serde_json::Map<String, Value> {
    let path = full;
    let text = std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("read {full}: {e}"));
    let value: Value = if path.ends_with(".toml") {
        toml::from_str(&text).unwrap_or_else(|e| panic!("parse {full}: {e}"))
    } else {
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {full}: {e}"))
    };
    match value {
        Value::Object(map) => map,
        _ => panic!("{full}: top level of a state file must be an object"),
    }
}

/// `<name>` 对应的文件（相对 `tests/resources/httpmock/`），toml / yaml / yml 只能有一种
fn find(name: &str) -> String {
    let found: Vec<String> = ["toml", "yaml", "yml"]
        .iter()
        .map(|ext| format!("{ROOT}/{name}.{ext}"))
        .filter(|path| std::path::Path::new(path).is_file())
        .collect();
    match found.as_slice() {
        [path] => path.clone(),
        [] => panic!("cassette {name} not found: {ROOT}/{name}.{{toml,yaml,yml}}"),
        _ => panic!("cassette {name} has more than one format: {found:?}"),
    }
}

async fn register_toml(
    server: &MockServer,
    state: &Arc<Mutex<Dynamic>>,
    path: &str,
    file: CassetteFile,
) {
    let base = server.base_url();
    let dir = dir_of(path);
    // 一个 cassette 一个引擎：脚本里 read_json / read_text 的相对路径相对这个 cassette 的目录
    let engine = Arc::new(build_engine(dir.clone()));
    for it in file.interactions {
        if it.request.path.is_none() && it.request.path_prefix.is_none() {
            panic!("{path} [{}]: request 要写 path 或 path_prefix", it.name);
        }
        let responder = match (it.response, it.responses, it.script) {
            (Some(one), None, None) => Responder::Sequence(vec![one]),
            (None, Some(many), None) if !many.is_empty() => Responder::Sequence(many),
            (None, None, Some(script)) => {
                let script_path = format!("{dir}/{script}");
                let ast = engine
                    .compile_file(script_path.clone().into())
                    .unwrap_or_else(|e| panic!("compile {script_path}: {e}"));
                Responder::Script(ast)
            }
            _ => panic!(
                "{path} [{}]: response / responses / script 只能写一个，responses 不能为空",
                it.name
            ),
        };
        let path_regex = it.request.path.as_deref().and_then(path_template);
        let handler = Handler {
            path_regex: path_regex.clone(),
            name: it.name,
            responder,
            calls: AtomicUsize::new(0),
            engine: engine.clone(),
            dir: dir.clone(),
            state: state.clone(),
            base: base.clone(),
        };
        let req = it.request;
        server
            .mock_async(move |when, then| {
                let mut when = when.method(req.method.as_str());
                match (&req.path, &path_regex) {
                    (Some(_), Some(regex)) => when = when.path_matches(regex.as_str()),
                    (Some(path), None) => when = when.path(path),
                    _ => {}
                }
                if let Some(prefix) = &req.path_prefix {
                    when = when.path_prefix(prefix);
                }
                for (k, v) in &req.query {
                    when = when.query_param(k, v);
                }
                for (k, v) in &req.headers {
                    when = when.header(k, v);
                }
                if let Some(json) = &req.json_body {
                    when = when.json_body(json.clone());
                }
                if let Some(json) = &req.json_body_includes {
                    when = when.json_body_includes(json.to_string());
                }
                for part in &req.body_includes {
                    when = when.body_includes(part);
                }
                let _ = when;
                then.respond_with(move |request| handler.respond(request));
            })
            .await;
    }
}

struct Handler {
    /// `path` 里有占位符时，用来从实际路径里取值
    path_regex: Option<regex::Regex>,
    name: String,
    responder: Responder,
    calls: AtomicUsize,
    engine: Arc<Engine>,
    state: Arc<Mutex<Dynamic>>,
    base: String,
    /// cassette 所在的目录，`body_file` 相对它
    dir: String,
}

impl Handler {
    fn respond(&self, request: &HttpMockRequest) -> HttpMockResponse {
        let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let spec = match &self.responder {
            Responder::Sequence(list) => list[(calls - 1).min(list.len() - 1)].clone(),
            Responder::Script(ast) => {
                let mut req = Req::from(request);
                if let Some(regex) = &self.path_regex {
                    if let Some(caps) = regex.captures(&req.path) {
                        req.params = regex
                            .capture_names()
                            .flatten()
                            .filter_map(|name| {
                                Some((name.to_string(), caps.name(name)?.as_str().to_string()))
                            })
                            .collect();
                    }
                }
                self.run_script(ast, req, calls)
            }
        };
        self.build(spec)
    }

    /// 跑脚本：state 放进 scope 给脚本改，跑完再取回来存上
    fn run_script(&self, ast: &AST, request: Req, calls: usize) -> ResponseSpec {
        let mut state = self.state.lock().unwrap();
        let mut scope = Scope::new();
        scope.push("request", request);
        scope.push("calls", calls as i64);
        scope.push("base", self.base.clone());
        scope.push_dynamic("state", state.clone());
        let result: Dynamic = self
            .engine
            .eval_ast_with_scope(&mut scope, ast)
            .unwrap_or_else(|e| panic!("[{}] script: {e}", self.name));
        *state = scope.get_value::<Dynamic>("state").unwrap();
        rhai::serde::from_dynamic(&result)
            .unwrap_or_else(|e| panic!("[{}] script result: {e}", self.name))
    }

    fn build(&self, spec: ResponseSpec) -> HttpMockResponse {
        let mut builder = HttpMockResponse::builder().status(spec.status);
        let mut has_content_type = false;
        for (k, v) in &spec.headers {
            has_content_type |= k.eq_ignore_ascii_case("content-type");
            builder = builder.header(k.as_str(), v.as_str());
        }
        let body: Vec<u8> = match (spec.body, spec.body_file, spec.json) {
            (Some(text), None, None) => text.replace("{{base}}", &self.base).into_bytes(),
            (None, Some(file), None) => {
                let path = format!("{}/{file}", self.dir);
                let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
                match String::from_utf8(bytes) {
                    Ok(text) => text.replace("{{base}}", &self.base).into_bytes(),
                    Err(e) => e.into_bytes(),
                }
            }
            (None, None, Some(json)) => {
                if !has_content_type {
                    builder = builder.header("content-type", "application/json");
                }
                json.to_string().into_bytes()
            }
            (None, None, None) => Vec::new(),
            _ => panic!("[{}] body / body_file / json 只能写一个", self.name),
        };
        builder.body(body).build()
    }
}

// ============================================================
// 脚本里的 request 和内置函数
// ============================================================

/// 脚本里的 `request`。字段只读，body 按需用方法解析：
///
/// | 写法 | 返回 |
/// |---|---|
/// | `request.method` / `request.path` | 字符串 |
/// | `request.param("id")` | `path` 里 `{id}` 占位符匹配到的值，没有是 `()` |
/// | `request.params` | 所有占位符的值 |
/// | `request.query("text")` | 第一个同名参数，没有是 `()` |
/// | `request.query_all("types")` | 所有同名参数的数组 |
/// | `request.queries` | 所有参数，同名的取第一个 |
/// | `request.header("X-Moji-Token")` | 请求头，名字不分大小写，没有是 `()` |
/// | `request.headers` | 所有请求头，名字是小写 |
/// | `request.content_type` | `content-type` 头，没有是空字符串 |
/// | `request.text()` | body 按 content-type 里的 charset 解码成文本，默认 UTF-8 |
/// | `request.json()` | body 按 JSON 解析，解析不了脚本报错 |
/// | `request.form()` | body 按 `application/x-www-form-urlencoded` 解析成 map，同名的取第一个 |
/// | `request.body_len` | body 字节数 |
#[derive(Clone)]
struct Req {
    method: String,
    path: String,
    /// `path` 占位符匹配到的值
    params: Vec<(String, String)>,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl From<&HttpMockRequest> for Req {
    fn from(request: &HttpMockRequest) -> Self {
        Self {
            method: request.method_str().to_string(),
            path: request.uri().path().to_string(),
            params: Vec::new(),
            query: request.query_params(),
            headers: request
                .headers_vec()
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                .collect(),
            body: request.body_vec(),
        }
    }
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    fn content_type(&self) -> &str {
        self.header("content-type").unwrap_or_default()
    }

    /// 按 content-type 里的 charset 解码。以后换成 hygiea 里 http client 同一套 decode_charset
    fn text(&self) -> String {
        let encoding = self
            .content_type()
            .split(';')
            .skip(1)
            .filter_map(|param| param.split_once('='))
            .find(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
            .and_then(|(_, value)| {
                encoding_rs::Encoding::for_label(value.trim().trim_matches('"').as_bytes())
            })
            .unwrap_or(encoding_rs::UTF_8);
        encoding.decode(&self.body).0.into_owned()
    }
}

fn str_or_unit(value: Option<&str>) -> Dynamic {
    value.map_or(Dynamic::UNIT, |v| v.to_string().into())
}

type ScriptResult = Result<Dynamic, Box<rhai::EvalAltResult>>;

/// 脚本引擎：注册 `request` 类型，和读数据文件的 `read_json(path)` / `read_text(path)`，
/// 相对路径相对 `dir`（cassette 所在的目录）
fn build_engine(dir: String) -> Engine {
    let json_dir = dir.clone();
    let mut engine = Engine::new();
    // 脚本里 `import "common" as c;` 引用同目录的 common.rhai，放几个接口共用的函数
    engine.set_module_resolver(rhai::module_resolvers::FileModuleResolver::new_with_path(
        &dir,
    ));
    engine
        .register_type_with_name::<Req>("Request")
        .register_get("method", |r: &mut Req| r.method.clone())
        .register_get("path", |r: &mut Req| r.path.clone())
        .register_get("content_type", |r: &mut Req| r.content_type().to_string())
        .register_get("body_len", |r: &mut Req| r.body.len() as i64)
        .register_get("queries", |r: &mut Req| {
            let mut map = rhai::Map::new();
            for (k, v) in r.query.iter().rev() {
                map.insert(k.as_str().into(), v.clone().into());
            }
            map
        })
        .register_get("headers", |r: &mut Req| {
            let mut map = rhai::Map::new();
            for (k, v) in r.headers.iter().rev() {
                map.insert(k.as_str().into(), v.clone().into());
            }
            map
        })
        .register_fn("param", |r: &mut Req, name: &str| {
            str_or_unit(
                r.params
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str()),
            )
        })
        .register_get("params", |r: &mut Req| {
            let mut map = rhai::Map::new();
            for (k, v) in &r.params {
                map.insert(k.as_str().into(), v.clone().into());
            }
            map
        })
        .register_fn("query", |r: &mut Req, name: &str| {
            str_or_unit(
                r.query
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str()),
            )
        })
        .register_fn("query_all", |r: &mut Req, name: &str| {
            r.query
                .iter()
                .filter(|(k, _)| k == name)
                .map(|(_, v)| Dynamic::from(v.clone()))
                .collect::<rhai::Array>()
        })
        .register_fn("header", |r: &mut Req, name: &str| {
            str_or_unit(r.header(name))
        })
        .register_fn("text", |r: &mut Req| r.text())
        .register_fn("json", |r: &mut Req| -> ScriptResult {
            let value: Value = serde_json::from_slice(&r.body)
                .map_err(|e| format!("request body is not JSON: {e}"))?;
            rhai::serde::to_dynamic(value)
        })
        .register_fn("form", |r: &mut Req| {
            let mut map = rhai::Map::new();
            for (k, v) in form_urlencoded::parse(&r.body) {
                map.entry(k.as_ref().into())
                    .or_insert_with(|| v.into_owned().into());
            }
            map
        })
        .register_fn("read_json", move |path: &str| -> ScriptResult {
            let full = format!("{json_dir}/{path}");
            let text = std::fs::read_to_string(&full).map_err(|e| format!("read {full}: {e}"))?;
            let value: Value =
                serde_json::from_str(&text).map_err(|e| format!("parse {full}: {e}"))?;
            rhai::serde::to_dynamic(value)
        })
        .register_fn("read_text", move |path: &str| -> ScriptResult {
            let full = format!("{dir}/{path}");
            let text = std::fs::read_to_string(&full).map_err(|e| format!("read {full}: {e}"))?;
            Ok(text.into())
        });
    engine
}
