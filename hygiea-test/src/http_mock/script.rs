//! Rhai 脚本：请求对象 `request` 和内置函数。
//!
//! 脚本里能用的变量：`request`、`state`（同一个 server 共用，改了会保留）、`calls`（这条 interaction
//! 第几次被调用，从 1 开始；一个请求走完整条 handler 链只算一次）、`base`（mock server 的地址）。
//! 响应应该由 state 和请求内容决定，像真服务器一样。`calls` 是特意留的口子，只给"同样的请求、
//! 服务端也没状态变化，结果却不一样"这种情况用（比如测 client 对上游偶发 503、超时的处理），
//! 别用它把几次调用关联起来做常规逻辑。配合 `nth(calls, [..])`：
//! 取列表第 n 个（从 1 开始）；n < 1 或超过列表长度脚本报错（这次请求 500，记一条问题），不循环也不停在最后一个。
//! 内置函数：`read_json(path)` / `read_text(path)` 读文件，`write_file(path, 字符串或 blob)` 写文件，
//! 读写的都是这个 server 的内存文件表（路径相对 `data/static/`），不落盘；
//! `import "common" as c;` 引用同目录的 `common.rhai`。
//!
//! `request` 的用法：
//!
//! | 写法 | 返回 |
//! |---|---|
//! | `request.method` / `request.path` | 字符串 |
//! | `request.param("id")` / `request.params` | `path` 里 `{id}` 占位符匹配到的值，没有是 `()` |
//! | `request.query("text")` | 第一个同名参数，没有是 `()` |
//! | `request.query_all("types")` | 所有同名参数的数组 |
//! | `request.queries` | 所有参数，同名的取第一个 |
//! | `request.queries_all` | 所有参数，值是数组：`#{ tag: ["a", "b"] }` |
//! | `request.header("X-Token")` / `request.headers` | 请求头，名字不分大小写，同名的取第一个；`headers` 的名字是小写 |
//! | `request.header_all("X-Token")` / `request.headers_all` | 同名请求头的所有值，数组 |
//! | `request.cookie("sid")` / `request.cookies` | cookie，同名的取第一个；没有是 `()` |
//! | `request.cookie_all("sid")` / `request.cookies_all` | 同名 cookie 的所有值，数组 |
//! | `request.content_type` | `content-type` 头，没有是空字符串 |
//! | `request.text()` | body 按 content-type 里的 charset 解码，默认 UTF-8 |
//! | `request.json()` | body 按 JSON 解析，解析不了脚本报错 |
//! | `request.form()` | body 按 `application/x-www-form-urlencoded` 解析，同名的取第一个 |
//! | `request.body()` | 原始 body，blob |
//! | `request.body_len` | body 字节数 |
//!
//! 时间是 `DateTime` 类型（底层 `time::UtcDateTime`），能放进 state，请求之间保持是对象：
//!
//! | 写法 | 返回 |
//! |---|---|
//! | `now_utc()` | 当前时刻 |
//! | `parse_rfc3339(s)` | 解析 RFC 3339，带 offset 的换算成 UTC；解析不了脚本报错 |
//! | `from_secs(n)` / `from_millis(n)` | Unix 秒 / 毫秒时间戳 |
//! | `t.to_rfc3339()` / `t.to_string()` | UTC 的 RFC 3339 字符串，比如 `2026-01-01T00:00:00Z` |
//! | `t.secs` / `t.millis` | Unix 秒 / 毫秒时间戳 |
//! | `t.shift_secs(n)` / `t.shift_days(n)` | 平移后的新时刻，n 可以是负数；越界脚本报错 |
//! | `a < b`、`a == b` 等比较 | 按时刻比较 |
//!
//! state 和 JSON 互转（加载 `data/state.json`、[`Mocked::state`](super::Mocked::state)、
//! [`Mocked::update_state`](super::Mocked::update_state)）：`DateTime` 转成 `to_rfc3339()` 的字符串；
//! 反过来 JSON 里的时间只是字符串，不会自动变成 `DateTime`，脚本里用 `parse_rfc3339` 转。
//! 脚本返回的响应里有 `DateTime` 也一样转成字符串

use std::sync::Arc;

use httpmock::prelude::HttpMockRequest;
use hygiea_core::HyErr;
use hygiea_core::datetime::{HygieaDateTimeExt, now_utc};
use rhai::{Blob, Dynamic, Engine, EvalAltResult, Scope};
use serde_json::Value;
use time::{Duration, UtcDateTime};

use super::handler::Shared;
use super::template::PathTemplate;

#[derive(Clone)]
pub(crate) struct Req {
    method: String,
    path: String,
    params: Vec<(String, String)>,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    pub fn new(request: &HttpMockRequest, template: &PathTemplate) -> Self {
        let path = request.uri().path().to_string();
        let params = template.params(&path).unwrap_or_default();
        Self {
            method: request.method_str().to_string(),
            path,
            params,
            query: request.query_params(),
            headers: request
                .headers_vec()
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                .collect(),
            body: request.body_vec(),
        }
    }

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

    /// 所有 `cookie` 头里的 `名=值`，按出现顺序；名字区分大小写
    fn cookies(&self) -> Vec<(String, String)> {
        self.headers
            .iter()
            .filter(|(k, _)| k == "cookie")
            .flat_map(|(_, v)| v.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// 按 content-type 里的 charset 解码，没写或不认识按 UTF-8
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

/// 同名的收成数组，按出现顺序
fn pairs_to_multi_map(pairs: &[(String, String)]) -> rhai::Map {
    let mut map = rhai::Map::new();
    for (k, v) in pairs {
        if let Some(values) = map
            .entry(k.as_str().into())
            .or_insert_with(|| Dynamic::from(rhai::Array::new()))
            .write_lock::<rhai::Array>()
            .as_deref_mut()
        {
            values.push(v.clone().into());
        }
    }
    map
}

fn pairs_to_map<'a>(pairs: impl DoubleEndedIterator<Item = &'a (String, String)>) -> rhai::Map {
    let mut map = rhai::Map::new();
    // 倒着插，同名的留第一个
    for (k, v) in pairs.rev() {
        map.insert(k.as_str().into(), v.clone().into());
    }
    map
}

type ScriptResult = Result<Dynamic, Box<EvalAltResult>>;
type DateTimeResult = Result<UtcDateTime, Box<EvalAltResult>>;

fn date_err(e: HyErr) -> Box<EvalAltResult> {
    e.to_string().into()
}

fn to_rfc3339(t: &mut UtcDateTime) -> Result<String, Box<EvalAltResult>> {
    t.format_ext_rfc3339().map_err(date_err)
}

/// `Dynamic` 转 JSON：rhai 自带的 serde 不认自定义类型，`DateTime` 在这里转成 RFC 3339 字符串，其他的交给 rhai
pub(crate) fn to_json(value: &Dynamic) -> Result<Value, String> {
    let value = match value.flatten_clone().try_cast_result::<UtcDateTime>() {
        Ok(t) => {
            return t
                .format_ext_rfc3339()
                .map(Value::String)
                .map_err(|e| e.to_string());
        }
        Err(value) => value,
    };
    let value = match value.try_cast_result::<rhai::Map>() {
        Ok(map) => {
            return map
                .iter()
                .map(|(k, v)| Ok((k.to_string(), to_json(v)?)))
                .collect::<Result<serde_json::Map<_, _>, String>>()
                .map(Value::Object);
        }
        Err(value) => value,
    };
    let value = match value.try_cast_result::<rhai::Array>() {
        Ok(list) => {
            return list
                .iter()
                .map(to_json)
                .collect::<Result<_, _>>()
                .map(Value::Array);
        }
        Err(value) => value,
    };
    rhai::serde::from_dynamic(&value).map_err(|e| e.to_string())
}

/// 从内存文件表读文本
fn read(shared: &Shared, path: &str) -> Result<String, Box<EvalAltResult>> {
    let key = super::file_key(path)?;
    let bytes = shared
        .files
        .lock()
        .get(&key)
        .cloned()
        .ok_or_else(|| format!("file not found: {key}"))?;
    String::from_utf8(bytes).map_err(|e| format!("{key} is not UTF-8: {e}").into())
}

fn write(shared: &Shared, path: &str, bytes: Vec<u8>) -> Result<(), Box<EvalAltResult>> {
    let key = super::file_key(path)?;
    shared.files.lock().insert(key, bytes);
    Ok(())
}

/// 一个服务一个引擎：`read_json` / `read_text` / `write_file` 读写内存文件表；
/// `import` 按发起 import 的脚本文件所在目录解析
/// 一次脚本最多执行多少步：正常脚本远用不完，写出死循环时中断报错（`ScriptFailed`），不让测试卡住
const MAX_OPERATIONS: u64 = 1_000_000;
/// 脚本和脚本函数里表达式最多嵌套几层
const MAX_EXPR_DEPTH: usize = 64;

/// handler 脚本里能用的外部变量
pub(crate) const HANDLER_VARS: [&str; 4] = ["request", "state", "calls", "base"];
/// match_script 里能用的外部变量（匹配时还没算调用次数，没有 `calls`）
pub(crate) const MATCH_VARS: [&str; 3] = ["request", "state", "base"];

/// 编译时用的 scope：只有变量名，值不重要。严格变量模式下编译器靠它知道哪些外部变量存在
pub(crate) fn compile_scope(vars: &[&str]) -> Scope<'static> {
    let mut scope = Scope::new();
    for var in vars {
        scope.push_dynamic(*var, Dynamic::UNIT);
    }
    scope
}

pub(crate) fn build_engine(shared: Arc<Shared>) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(MAX_OPERATIONS);
    // 表达式嵌套深度：rhai 在 debug 构建（测试就是）下默认只有 32，函数体里只有 16，
    // 函数里写两层 for 加个 if 就会编译失败（Expression exceeds maximum complexity），统一放宽到 64
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_EXPR_DEPTH);
    // 严格变量：用了没定义的变量（拼错 state、request……）编译时就报错，不用等请求跑到那一行
    engine.set_strict_variables(true);
    engine.set_module_resolver(rhai::module_resolvers::FileModuleResolver::new());
    let (json_shared, text_shared, str_shared) = (shared.clone(), shared.clone(), shared.clone());
    let blob_shared = shared;
    register_datetime(&mut engine);
    engine
        .register_type_with_name::<Req>("Request")
        .register_get("method", |r: &mut Req| r.method.clone())
        .register_get("path", |r: &mut Req| r.path.clone())
        .register_get("content_type", |r: &mut Req| r.content_type().to_string())
        .register_get("body_len", |r: &mut Req| r.body.len() as i64)
        .register_get("params", |r: &mut Req| pairs_to_map(r.params.iter()))
        .register_get("queries", |r: &mut Req| pairs_to_map(r.query.iter()))
        .register_get("headers", |r: &mut Req| pairs_to_map(r.headers.iter()))
        .register_get("queries_all", |r: &mut Req| pairs_to_multi_map(&r.query))
        .register_get("headers_all", |r: &mut Req| pairs_to_multi_map(&r.headers))
        .register_fn("param", |r: &mut Req, name: &str| {
            str_or_unit(
                r.params
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str()),
            )
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
        .register_fn("header_all", |r: &mut Req, name: &str| {
            let name = name.to_ascii_lowercase();
            r.headers
                .iter()
                .filter(|(k, _)| *k == name)
                .map(|(_, v)| Dynamic::from(v.clone()))
                .collect::<rhai::Array>()
        })
        .register_get("cookies", |r: &mut Req| pairs_to_map(r.cookies().iter()))
        .register_get("cookies_all", |r: &mut Req| {
            pairs_to_multi_map(&r.cookies())
        })
        .register_fn("cookie", |r: &mut Req, name: &str| {
            let cookies = r.cookies();
            str_or_unit(
                cookies
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str()),
            )
        })
        .register_fn("cookie_all", |r: &mut Req, name: &str| {
            r.cookies()
                .into_iter()
                .filter(|(k, _)| k == name)
                .map(|(_, v)| Dynamic::from(v))
                .collect::<rhai::Array>()
        })
        .register_fn("text", |r: &mut Req| r.text())
        .register_fn("json", |r: &mut Req| -> ScriptResult {
            let value: Value = serde_json::from_slice(&r.body)
                .map_err(|e| format!("request body is not JSON: {e}"))?;
            rhai::serde::to_dynamic(value)
        })
        .register_fn("body", |r: &mut Req| -> Blob { r.body.clone() })
        .register_fn("form", |r: &mut Req| {
            let mut map = rhai::Map::new();
            for (k, v) in form_urlencoded::parse(&r.body) {
                map.entry(k.as_ref().into())
                    .or_insert_with(|| v.into_owned().into());
            }
            map
        })
        .register_fn("nth", |n: i64, list: rhai::Array| -> ScriptResult {
            let len = list.len();
            usize::try_from(n)
                .ok()
                .filter(|&n| n >= 1)
                .and_then(|n| list.into_iter().nth(n - 1))
                .ok_or_else(|| format!("nth: no item #{n}, the list has {len} item(s)").into())
        })
        .register_fn("read_json", move |path: &str| -> ScriptResult {
            let text = read(&json_shared, path)?;
            let value: Value =
                serde_json::from_str(&text).map_err(|e| format!("parse {path}: {e}"))?;
            rhai::serde::to_dynamic(value)
        })
        .register_fn("read_text", move |path: &str| -> ScriptResult {
            Ok(read(&text_shared, path)?.into())
        })
        .register_fn(
            "write_file",
            move |path: &str, text: &str| -> Result<(), Box<EvalAltResult>> {
                write(&str_shared, path, text.as_bytes().to_vec())
            },
        )
        .register_fn(
            "write_file",
            move |path: &str, blob: Blob| -> Result<(), Box<EvalAltResult>> {
                write(&blob_shared, path, blob)
            },
        );
    engine
}

/// `DateTime` 类型和它的方法，见文件开头
fn register_datetime(engine: &mut Engine) {
    engine
        .register_type_with_name::<UtcDateTime>("DateTime")
        .register_fn("now_utc", now_utc)
        .register_fn("parse_rfc3339", |s: &str| -> DateTimeResult {
            UtcDateTime::parse_ext_rfc3339(s).map_err(date_err)
        })
        .register_fn("from_secs", |n: i64| -> DateTimeResult {
            UtcDateTime::from_secs(n).map_err(date_err)
        })
        .register_fn("from_millis", |n: i64| -> DateTimeResult {
            UtcDateTime::from_millis(n).map_err(date_err)
        })
        .register_fn("to_rfc3339", to_rfc3339)
        .register_fn("to_string", to_rfc3339)
        .register_fn("to_debug", to_rfc3339)
        .register_get("secs", |t: &mut UtcDateTime| t.unix_timestamp())
        .register_get("millis", |t: &mut UtcDateTime| {
            (t.unix_timestamp_nanos() / 1_000_000) as i64
        })
        .register_fn(
            "shift_secs",
            |t: &mut UtcDateTime, n: i64| -> DateTimeResult {
                t.shift(Duration::seconds(n)).map_err(date_err)
            },
        )
        .register_fn(
            "shift_days",
            |t: &mut UtcDateTime, n: i64| -> DateTimeResult {
                // Duration::days 溢出会 panic，先自己乘
                let secs = n
                    .checked_mul(86_400)
                    .ok_or_else(|| format!("shift_days: {n} days out of range"))?;
                t.shift(Duration::seconds(secs)).map_err(date_err)
            },
        )
        .register_fn("==", |a: UtcDateTime, b: UtcDateTime| a == b)
        .register_fn("!=", |a: UtcDateTime, b: UtcDateTime| a != b)
        .register_fn("<", |a: UtcDateTime, b: UtcDateTime| a < b)
        .register_fn("<=", |a: UtcDateTime, b: UtcDateTime| a <= b)
        .register_fn(">", |a: UtcDateTime, b: UtcDateTime| a > b)
        .register_fn(">=", |a: UtcDateTime, b: UtcDateTime| a >= b);
}
