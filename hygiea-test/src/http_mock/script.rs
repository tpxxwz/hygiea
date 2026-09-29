//! Rhai 脚本：请求对象 `request` 和内置函数。
//!
//! 脚本里能用的变量：`request`、`state`（同一个 server 共用，改了会保留）、`calls`（这条 interaction
//! 第几次被调用，从 1 开始）、`base`（mock server 的地址）。
//! 内置函数：`read_json(path)` / `read_text(path)` 读数据文件；`import "common" as c;` 引用同目录的 `common.rhai`。
//! 相对路径都相对 cassette 所在的目录。
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
//! | `request.header("X-Token")` / `request.headers` | 请求头，名字不分大小写；`headers` 的名字是小写 |
//! | `request.cookie("sid")` | cookie，没有是 `()` |
//! | `request.content_type` | `content-type` 头，没有是空字符串 |
//! | `request.text()` | body 按 content-type 里的 charset 解码，默认 UTF-8 |
//! | `request.json()` | body 按 JSON 解析，解析不了脚本报错 |
//! | `request.form()` | body 按 `application/x-www-form-urlencoded` 解析，同名的取第一个 |
//! | `request.body_len` | body 字节数 |

use httpmock::prelude::HttpMockRequest;
use rhai::{Dynamic, Engine, EvalAltResult};
use serde_json::Value;

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
    pub fn new(request: &HttpMockRequest, template: Option<&PathTemplate>) -> Self {
        let path = request.uri().path().to_string();
        let params = template.and_then(|t| t.params(&path)).unwrap_or_default();
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

    fn cookie(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .filter(|(k, _)| k == "cookie")
            .flat_map(|(_, v)| v.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
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

fn pairs_to_map<'a>(pairs: impl DoubleEndedIterator<Item = &'a (String, String)>) -> rhai::Map {
    let mut map = rhai::Map::new();
    // 倒着插，同名的留第一个
    for (k, v) in pairs.rev() {
        map.insert(k.as_str().into(), v.clone().into());
    }
    map
}

type ScriptResult = Result<Dynamic, Box<EvalAltResult>>;

fn read(dir: &str, path: &str) -> Result<String, Box<EvalAltResult>> {
    let full = format!("{dir}/{path}");
    std::fs::read_to_string(&full).map_err(|e| format!("read {full}: {e}").into())
}

/// 一个 cassette 一个引擎：`read_json` / `read_text` 的相对路径相对 `dir`（cassette 所在的目录）；
/// `import` 按发起 import 的脚本文件所在目录解析，case 复用 `../server/` 的脚本时，
/// 脚本里的 `import "common"` 仍然找 `server/common.rhai`
pub(crate) fn build_engine(dir: &str) -> Engine {
    let mut engine = Engine::new();
    engine.set_module_resolver(rhai::module_resolvers::FileModuleResolver::new());
    let (json_dir, text_dir) = (dir.to_string(), dir.to_string());
    engine
        .register_type_with_name::<Req>("Request")
        .register_get("method", |r: &mut Req| r.method.clone())
        .register_get("path", |r: &mut Req| r.path.clone())
        .register_get("content_type", |r: &mut Req| r.content_type().to_string())
        .register_get("body_len", |r: &mut Req| r.body.len() as i64)
        .register_get("params", |r: &mut Req| pairs_to_map(r.params.iter()))
        .register_get("queries", |r: &mut Req| pairs_to_map(r.query.iter()))
        .register_get("headers", |r: &mut Req| pairs_to_map(r.headers.iter()))
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
        .register_fn("cookie", |r: &mut Req, name: &str| {
            str_or_unit(r.cookie(name).as_deref())
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
            let text = read(&json_dir, path)?;
            let value: Value =
                serde_json::from_str(&text).map_err(|e| format!("parse {json_dir}/{path}: {e}"))?;
            rhai::serde::to_dynamic(value)
        })
        .register_fn("read_text", move |path: &str| -> ScriptResult {
            Ok(read(&text_dir, path)?.into())
        });
    engine
}
