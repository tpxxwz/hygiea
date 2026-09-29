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
    pub response: Option<ResponseSpec>,
    pub responses: Option<Vec<ResponseSpec>>,
    pub script: Option<String>,
    /// Rhai 脚本，返回 true 才算匹配；在声明式条件都满足之后再判断
    pub match_script: Option<String>,
    /// 每次响应前先等这么久（毫秒），测超时用
    pub delay_ms: Option<u64>,
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
    /// 路径完全相等，可以带 `{名字}` 占位符（转成正则匹配，脚本里用 `request.param` 取）
    pub path: Option<String>,
    #[serde(default)]
    path_not: OneOrMany<String>,
    #[serde(default)]
    path_includes: OneOrMany<String>,
    #[serde(default)]
    path_excludes: OneOrMany<String>,
    #[serde(default)]
    pub path_prefix: OneOrMany<String>,
    #[serde(default)]
    path_suffix: OneOrMany<String>,
    #[serde(default)]
    pub path_matches: OneOrMany<String>,

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

/// 翻译成 httpmock 的匹配条件前先检查、编译好的部分
pub(crate) struct CompiledRequest {
    spec: RequestSpec,
    method: http::Method,
    /// `path` 带占位符时的模板
    pub template: Option<PathTemplate>,
    path_matches: Vec<regex::Regex>,
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
    /// 检查并编译正则、路径模板；`path` 和各 `path_*` 至少要写一个
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
        let template = match &self.path {
            Some(path) if path.contains('{') => Some(PathTemplate::new(path)?),
            _ => None,
        };
        let path_matches = std::mem::take(&mut self.path_matches)
            .into_vec()
            .iter()
            .map(|p| regex(p))
            .collect::<Result<Vec<_>, _>>()?;
        let query_param_matches = pairs(&mut self.query_param_matches)?;
        let header_matches = pairs(&mut self.header_matches)?;
        let body_matches = std::mem::take(&mut self.body_matches)
            .into_vec()
            .iter()
            .map(|p| regex(p))
            .collect::<Result<Vec<_>, _>>()?;
        let has_path_prefix = matches!(&self.path_prefix, OneOrMany::One(_))
            || matches!(&self.path_prefix, OneOrMany::Many(v) if !v.is_empty());
        if self.path.is_none() && !has_path_prefix && path_matches.is_empty() {
            return Err(invalid(
                "request needs `path`, `path_prefix` or `path_matches`".into(),
            ));
        }
        Ok(CompiledRequest {
            spec: self,
            method,
            template,
            path_matches,
            query_param_matches,
            header_matches,
            body_matches,
        })
    }
}

impl CompiledRequest {
    /// 把匹配条件加到 httpmock 的 `When` 上
    pub fn apply(self, when: When) -> When {
        let s = self.spec;
        let mut when = when.method(self.method.as_str());
        match (s.path, &self.template) {
            (Some(_), Some(template)) => when = when.path_matches(template.regex.clone()),
            (Some(path), None) => when = when.path(path),
            (None, _) => {}
        }
        for v in s.path_not.into_vec() {
            when = when.path_not(v);
        }
        for v in s.path_includes.into_vec() {
            when = when.path_includes(v);
        }
        for v in s.path_excludes.into_vec() {
            when = when.path_excludes(v);
        }
        for v in s.path_prefix.into_vec() {
            when = when.path_prefix(v);
        }
        for v in s.path_suffix.into_vec() {
            when = when.path_suffix(v);
        }
        for r in self.path_matches {
            when = when.path_matches(r);
        }

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
