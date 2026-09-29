//! cassette 的 TOML 格式，以及请求匹配条件翻译成 httpmock 的 `When`。
//!
//! 匹配字段和 httpmock 的方法同名（`header_exists`、`query_param_matches` ……），看 httpmock 的文档就能写。
//! 几种参数形状：
//! - 单个值或数组：`header_exists = "x-token"` / `header_exists = ["a", "b"]`
//! - 键值对：表格 `header = { authorization = "Bearer t" }`，或者数组 `header = [{ name = "..", value = ".." }]`
//!   （同名的要写多个时用数组）
//! - `json_body_includes` / `json_body_excludes`：一个 JSON 对象，或者数组表示多个部分匹配

use std::collections::BTreeMap;

use httpmock::When;
use hygiea::{HyErr, err};
use serde::Deserialize;
use serde_json::Value;

use super::error::HttpMockErr;
use super::template::PathTemplate;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CassetteFile {
    #[serde(default)]
    pub interactions: Vec<Interaction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Interaction {
    pub name: String,
    pub request: RequestSpec,
    /// 单个 handler；和 `handlers` 二选一
    pub handler: Option<HandlerSpec>,
    /// handler 链，按顺序执行，至少一个；和 `handler` 二选一
    pub handlers: Option<Vec<HandlerSpec>>,
}

/// toml 里的一个 handler：先等 `delay_ms`，再 `script` / `response` 二选一。
/// 用 Option 是为了 `deny_unknown_fields`（和 flatten 的 enum 不能一起用），解析完马上转成 [`HandlerDef`]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandlerSpec {
    /// 毫秒，不写或 0 不等；负数、小数解析时就报错
    #[serde(default)]
    pub delay_ms: u64,
    /// 脚本，路径相对 `rhai/`；返回 `()` 交给下一个 handler
    pub script: Option<String>,
    /// 固定响应
    pub response: Option<ResponseSpec>,
}

impl HandlerSpec {
    fn into_def(self) -> Result<HandlerDef, &'static str> {
        let action = match (self.script, self.response) {
            (Some(script), None) => Action::Script(script),
            (None, Some(response)) => Action::Response(response),
            _ => return Err("exactly one of script / response is required"),
        };
        Ok(HandlerDef {
            delay_ms: self.delay_ms,
            action,
        })
    }
}

/// 校验过的 handler：toml 解析完转出来的，或者代码（[`Handler`](super::Handler)）直接构造的
pub(crate) struct HandlerDef {
    pub delay_ms: u64,
    pub action: Action,
}

pub(crate) enum Action {
    /// `rhai/` 下的脚本文件
    Script(String),
    /// 行内脚本，只有代码定义的路由有
    InlineScript(String),
    Response(ResponseSpec),
}

/// 校验过的路由：handler / handlers 已经统一成链（空链在编译路由时报错）
pub(crate) struct RouteDef {
    pub name: String,
    pub request: RequestSpec,
    pub handlers: Vec<HandlerDef>,
}

impl Interaction {
    /// 检查 handler / handlers 二选一、每个 handler 的 script / response 二选一；错误带上规则名
    pub fn into_def(self) -> Result<RouteDef, String> {
        let name = self.name;
        let specs = match (self.handler, self.handlers) {
            (Some(one), None) => vec![one],
            (None, Some(many)) => many,
            _ => {
                return Err(format!(
                    "[{name}] exactly one of handler / handlers is required"
                ));
            }
        };
        let handlers = specs
            .into_iter()
            .enumerate()
            .map(|(i, spec)| {
                spec.into_def()
                    .map_err(|e| format!("[{name}] handler #{}: {e}", i + 1))
            })
            .collect::<Result<_, _>>()?;
        Ok(RouteDef {
            name,
            request: self.request,
            handlers,
        })
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResponseSpec {
    #[serde(default = "default_status")]
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
    pub body_file: Option<String>,
    pub json: Option<Value>,
}

fn default_status() -> u16 {
    200
}

/// 单个值或数组
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(one) => vec![one],
            Self::Many(many) => many,
        }
    }
}

impl<T> Default for OneOrMany<T> {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NameValue {
    name: String,
    value: String,
}

/// 键值对：表格或 `[{ name, value }]` 数组
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum Pairs {
    Table(BTreeMap<String, String>),
    List(Vec<NameValue>),
}

impl Pairs {
    fn into_vec(self) -> Vec<(String, String)> {
        match self {
            Self::Table(map) => map.into_iter().collect(),
            Self::List(list) => list.into_iter().map(|p| (p.name, p.value)).collect(),
        }
    }
}

impl Default for Pairs {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestSpec {
    pub method: String,
    /// 路径，可以带 `{名字}` / `{名字:正则}` 变量（脚本里用 `request.param` 取），见 `template.rs`。
    /// 路由由 method + path 决定，同一个 method 下不能有两条 path 能匹配同一个路径（字面量除外，字面量优先）
    pub path: String,
    /// Rhai 脚本（相对 `rhai/`），返回 true 才算匹配；在声明式条件都满足之后再判断
    pub match_script: Option<String>,

    #[serde(default)]
    query_param: Pairs,
    #[serde(default)]
    query_param_exists: OneOrMany<String>,
    #[serde(default)]
    query_param_missing: OneOrMany<String>,
    #[serde(default)]
    query_param_includes: Pairs,
    #[serde(default)]
    query_param_matches: Pairs,

    #[serde(default)]
    header: Pairs,
    #[serde(default)]
    header_exists: OneOrMany<String>,
    #[serde(default)]
    header_missing: OneOrMany<String>,
    #[serde(default)]
    header_includes: Pairs,
    #[serde(default)]
    header_matches: Pairs,

    #[serde(default)]
    cookie: Pairs,
    #[serde(default)]
    cookie_exists: OneOrMany<String>,
    #[serde(default)]
    cookie_missing: OneOrMany<String>,

    body: Option<String>,
    #[serde(default)]
    body_includes: OneOrMany<String>,
    #[serde(default)]
    body_excludes: OneOrMany<String>,
    #[serde(default)]
    body_matches: OneOrMany<String>,

    json_body: Option<Value>,
    json_body_includes: Option<Value>,
    json_body_excludes: Option<Value>,

    #[serde(default)]
    form_urlencoded_tuple: Pairs,
    #[serde(default)]
    form_urlencoded_tuple_exists: OneOrMany<String>,
}

impl RequestSpec {
    /// 代码定义的路由：只有 method + path
    pub fn new(method: &str, path: &str) -> Self {
        Self {
            method: method.to_string(),
            path: path.to_string(),
            ..Default::default()
        }
    }
}

/// 翻译成 httpmock 的匹配条件前先检查、编译好的部分
pub(crate) struct CompiledRequest {
    spec: RequestSpec,
    pub method: http::Method,
    pub template: PathTemplate,
    query_param_matches: Vec<(regex::Regex, regex::Regex)>,
    header_matches: Vec<(regex::Regex, regex::Regex)>,
    body_matches: Vec<regex::Regex>,
}

/// JSON 部分匹配：一个对象是一个，数组是多个
fn json_parts(value: Option<Value>) -> Vec<String> {
    match value {
        None => Vec::new(),
        Some(Value::Array(parts)) => parts.iter().map(Value::to_string).collect(),
        Some(one) => vec![one.to_string()],
    }
}

impl RequestSpec {
    /// 检查并编译正则、路径模板
    pub fn compile(mut self, cassette: &str, name: &str) -> Result<CompiledRequest, HyErr> {
        let invalid = |cause: String| err!(HttpMockErr::InvalidCassette, { "path": cassette, "cause": format!("[{name}] {cause}") });
        let method = self
            .method
            .parse::<http::Method>()
            .map_err(|e| invalid(format!("method `{}`: {e}", self.method)))?;
        let regex = |pattern: &str| {
            regex::Regex::new(pattern).map_err(|e| invalid(format!("regex `{pattern}`: {e}")))
        };
        let pairs = |pairs: &mut Pairs| -> Result<Vec<(regex::Regex, regex::Regex)>, HyErr> {
            std::mem::take(pairs)
                .into_vec()
                .iter()
                .map(|(k, v)| Ok((regex(k)?, regex(v)?)))
                .collect()
        };
        let template = PathTemplate::new(&self.path)?;
        let query_param_matches = pairs(&mut self.query_param_matches)?;
        let header_matches = pairs(&mut self.header_matches)?;
        let body_matches = std::mem::take(&mut self.body_matches)
            .into_vec()
            .iter()
            .map(|p| regex(p))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CompiledRequest {
            spec: self,
            method,
            template,
            query_param_matches,
            header_matches,
            body_matches,
        })
    }
}

impl CompiledRequest {
    pub fn path(&self) -> &str {
        &self.spec.path
    }

    /// 把匹配条件加到 httpmock 的 `When` 上
    pub fn apply(self, when: When) -> When {
        let s = self.spec;
        let mut when = when.method(self.method.as_str());
        when = if self.template.literal {
            when.path(s.path)
        } else {
            when.path_matches(self.template.regex.clone())
        };

        for (k, v) in s.query_param.into_vec() {
            when = when.query_param(k, v);
        }
        for k in s.query_param_exists.into_vec() {
            when = when.query_param_exists(k);
        }
        for k in s.query_param_missing.into_vec() {
            when = when.query_param_missing(k);
        }
        for (k, v) in s.query_param_includes.into_vec() {
            when = when.query_param_includes(k, v);
        }
        for (k, v) in self.query_param_matches {
            when = when.query_param_matches(k, v);
        }

        for (k, v) in s.header.into_vec() {
            when = when.header(k, v);
        }
        for k in s.header_exists.into_vec() {
            when = when.header_exists(k);
        }
        for k in s.header_missing.into_vec() {
            when = when.header_missing(k);
        }
        for (k, v) in s.header_includes.into_vec() {
            when = when.header_includes(k, v);
        }
        for (k, v) in self.header_matches {
            when = when.header_matches(k, v);
        }

        for (k, v) in s.cookie.into_vec() {
            when = when.cookie(k, v);
        }
        for k in s.cookie_exists.into_vec() {
            when = when.cookie_exists(k);
        }
        for k in s.cookie_missing.into_vec() {
            when = when.cookie_missing(k);
        }

        if let Some(body) = s.body {
            when = when.body(body);
        }
        for v in s.body_includes.into_vec() {
            when = when.body_includes(v);
        }
        for v in s.body_excludes.into_vec() {
            when = when.body_excludes(v);
        }
        for r in self.body_matches {
            when = when.body_matches(r);
        }

        if let Some(json) = s.json_body {
            when = when.json_body(json);
        }
        for part in json_parts(s.json_body_includes) {
            when = when.json_body_includes(part);
        }
        for part in json_parts(s.json_body_excludes) {
            when = when.json_body_excludes(part);
        }

        for (k, v) in s.form_urlencoded_tuple.into_vec() {
            when = when.form_urlencoded_tuple(k, v);
        }
        for k in s.form_urlencoded_tuple_exists.into_vec() {
            when = when.form_urlencoded_tuple_exists(k);
        }
        when
    }
}
