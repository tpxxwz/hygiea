//! 用代码定义路由：[`Handler`]、[`Response`]，配合 [`Cassette::route`](super::Cassette::route) /
//! [`Cassette::route_chain`](super::Cassette::route_chain)。和 `routes.toml` 里的 handler 一一对应。
//!
//! ```ignore
//! Cassette::new()
//!     .state(json!({ "token": "t" }))
//!     .route("GET", "/api/items/{id}", Handler::script_str(r#"#{ json: #{ id: request.param("id") } }"#))
//!     .route("GET", "/api/down", Handler::response(Response::text("bad gateway").status(502)).delay_ms(300))
//!     .start()
//!     .await?;
//! ```

use std::collections::BTreeMap;

use serde_json::Value;

use super::spec::{Action, HandlerDef, ResponseSpec};

/// 固定响应，对应 toml 里的 `response = { .. }`：默认 200，body 是 json / 文本 / 文件表里的文件之一
#[derive(Clone)]
pub struct Response(pub(crate) ResponseSpec);

impl Response {
    fn new(body: Option<String>, body_file: Option<String>, json: Option<Value>) -> Self {
        Self(ResponseSpec {
            status: 200,
            headers: BTreeMap::new(),
            body,
            body_file,
            json,
        })
    }

    /// JSON，自动补 `content-type: application/json`
    pub fn json(value: impl Into<Value>) -> Self {
        Self::new(None, None, Some(value.into()))
    }

    /// 文本，里面的 `{{base}}` 换成 mock server 的地址；content-type 自己用 [`header`](Self::header) 加
    pub fn text(body: impl Into<String>) -> Self {
        Self::new(Some(body.into()), None, None)
    }

    /// 文件表里的文件，路径相对 `data/static/`（或 [`Cassette::file`](super::Cassette::file) 加的）
    pub fn file(path: impl Into<String>) -> Self {
        Self::new(None, Some(path.into()), None)
    }

    /// 空 body
    pub fn empty() -> Self {
        Self::new(None, None, None)
    }

    pub fn status(mut self, status: u16) -> Self {
        self.0.status = status;
        self
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.headers.insert(name.into(), value.into());
        self
    }
}

/// 一个 handler，对应 toml 里的 `handler = { .. }`：先等 `delay_ms`，再跑脚本或返回固定响应
pub struct Handler(pub(crate) HandlerDef);

impl Handler {
    fn new(action: Action) -> Self {
        Self(HandlerDef {
            delay_ms: 0,
            action,
        })
    }

    /// 服务目录 `rhai/` 下的脚本；只有 [`Cassette::load`](super::Cassette::load) 的服务能用
    pub fn script(path: impl Into<String>) -> Self {
        Self::new(Action::Script(path.into()))
    }

    /// 行内脚本，和文件里的写法一样；不要用 `import`（没有所在目录）
    pub fn script_str(code: impl Into<String>) -> Self {
        Self::new(Action::InlineScript(code.into()))
    }

    pub fn response(response: Response) -> Self {
        Self::new(Action::Response(response.0))
    }

    /// 先等这么久（毫秒）再执行，阻塞等待，同 toml 里的 `delay_ms`
    pub fn delay_ms(mut self, ms: u64) -> Self {
        self.0.delay_ms = ms;
        self
    }
}
