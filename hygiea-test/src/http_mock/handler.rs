//! 一条 interaction 收到请求后怎么响应：按顺序跑 handler 链，每一步先等 `delay_ms`，再固定响应或跑脚本；
//! 脚本返回 `()` 交给下一步，第一个给出响应的生效。组装成 httpmock 的响应；
//! 出了问题不 panic（mock server 里 panic 只会让客户端断开）：变成 [`HttpMockRuntimeErr`] 记进 problems，
//! 同时作为响应返回（见 [`Shared::fail`]）

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use httpmock::prelude::{HttpMockRequest, HttpMockResponse};
use hygiea_core::{HyErr, err};
use parking_lot::Mutex;
use rhai::{AST, Dynamic, Engine, Scope};

use super::error::HttpMockRuntimeErr;
use super::script::{Req, to_json};
use super::spec::ResponseSpec;
use super::template::PathTemplate;

/// 组装好的响应：状态码、响应头、body
type Built = (u16, Vec<(String, String)>, Vec<u8>);

pub(crate) enum Step {
    Fixed(ResponseSpec),
    Script(AST),
}

/// handler 链里的一步
pub(crate) struct Stage {
    /// 先等这么久（毫秒）再执行；在回调里阻塞等待，等的时候这个 server 处理不了别的请求
    pub delay_ms: u64,
    pub step: Step,
    /// 报错时用：`script login.rhai` / `inline script` / `response`
    pub desc: String,
}

/// 同一个 server 上所有 interaction 共用的东西
pub(crate) struct Shared {
    pub state: Mutex<Dynamic>,
    pub problems: Mutex<Vec<HyErr>>,
    /// 内存文件表，key 是相对 root 的路径
    pub files: Mutex<HashMap<String, Vec<u8>>>,
    pub base: String,
}

/// mock 自己出问题时的响应头，值是错误码；和模拟出来的服务端错误区分开
pub const PROBLEM_HEADER: &str = "x-hygiea-mock-problem";

impl Shared {
    pub fn problem(&self, err: HyErr) {
        self.problems.lock().push(err);
    }

    /// 记一条问题，同时把它作为这次请求的响应：`{"err_code":..,"message":..}`，带上 [`PROBLEM_HEADER`]
    pub fn fail(&self, status: u16, err: HyErr) -> HttpMockResponse {
        let body = serde_json::json!({ "err_code": err.err_code(), "message": format!("{err:#}") });
        let response = HttpMockResponse::builder()
            .status(status)
            .header("content-type", "application/json")
            .header(PROBLEM_HEADER, err.err_code())
            .body(body.to_string())
            .build();
        self.problem(err);
        response
    }
}

pub(crate) struct RouteHandler {
    pub cassette: String,
    pub name: String,
    pub template: PathTemplate,
    pub chain: Vec<Stage>,
    pub match_script: Option<AST>,
    pub engine: Arc<Engine>,
    pub shared: Arc<Shared>,
    /// 这条 interaction 被调用了几次，脚本里的 `calls`
    pub calls: AtomicUsize,
}

impl RouteHandler {
    /// 问题报告里定位用：cassette、规则名、这次请求
    fn route(&self, request: &HttpMockRequest) -> String {
        format!(
            "{} [{}] {} {}",
            self.cassette,
            self.name,
            request.method_str(),
            request.uri().path()
        )
    }

    /// `match_script`：返回 true 才算匹配；出错按不匹配处理，并记一条问题
    pub fn matches(&self, request: &HttpMockRequest) -> bool {
        let Some(ast) = &self.match_script else {
            return true;
        };
        let req = Req::new(request, &self.template);
        let state = self.shared.state.lock().clone();
        let mut scope = Scope::new();
        scope.push("request", req);
        scope.push_dynamic("state", state);
        scope.push("base", self.shared.base.clone());
        match self.engine.eval_ast_with_scope::<bool>(&mut scope, ast) {
            Ok(matched) => matched,
            Err(e) => {
                self.shared.problem(
                    err!(HttpMockRuntimeErr::MatchScriptFailed, { "route": self.route(request) })
                        .with_source(e.to_string()),
                );
                false
            }
        }
    }

    pub fn respond(&self, request: &HttpMockRequest) -> HttpMockResponse {
        let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let route = format!("{} call #{calls}", self.route(request));
        let req = Req::new(request, &self.template);
        let built = self
            .run_chain(req, calls, &route)
            .and_then(|spec| self.build(spec, &route));
        let (status, headers, body) = match built {
            Ok(built) => built,
            Err(e) => return self.shared.fail(500, e),
        };
        let mut builder = HttpMockResponse::builder().status(status);
        for (k, v) in headers {
            builder = builder.header(k, v);
        }
        builder.body(body).build()
    }

    /// 按顺序跑 handler 链，第一个给出响应的生效
    fn run_chain(&self, req: Req, calls: usize, route: &str) -> Result<ResponseSpec, HyErr> {
        for (i, stage) in self.chain.iter().enumerate() {
            if stage.delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(stage.delay_ms));
            }
            let spec = match &stage.step {
                Step::Fixed(spec) => Some(spec.clone()),
                Step::Script(ast) => self.run_script(ast, req.clone(), calls, route, i + 1)?,
            };
            if let Some(spec) = spec {
                return Ok(spec);
            }
        }
        let steps: Vec<String> = self
            .chain
            .iter()
            .enumerate()
            .map(|(i, stage)| format!("#{} {}", i + 1, stage.desc))
            .collect();
        Err(err!(HttpMockRuntimeErr::NoResponse, { "route": route, "steps": steps.join(", ") }))
    }

    /// 跑脚本：state 放进 scope 给脚本改，跑完再取回来存上；脚本返回 `()` 表示交给下一个 handler
    fn run_script(
        &self,
        ast: &AST,
        req: Req,
        calls: usize,
        route: &str,
        step: usize,
    ) -> Result<Option<ResponseSpec>, HyErr> {
        let mut state = self.shared.state.lock();
        let mut scope = Scope::new();
        scope.push("request", req);
        scope.push("calls", calls as i64);
        scope.push("base", self.shared.base.clone());
        scope.push_dynamic("state", state.clone());
        let result: Dynamic = self
            .engine
            .eval_ast_with_scope(&mut scope, ast)
            .map_err(|e| {
                err!(HttpMockRuntimeErr::ScriptFailed, { "route": route, "step": step })
                    .with_source(e.to_string())
            })?;
        if let Some(new_state) = scope.get_value::<Dynamic>("state") {
            *state = new_state;
        }
        if result.is_unit() {
            return Ok(None);
        }
        let invalid = |e: String| err!(HttpMockRuntimeErr::InvalidResponse, { "route": route, "cause": format!("handler #{step} returned {e}") });
        let result = to_json(&result).map_err(invalid)?;
        serde_json::from_value(result)
            .map(Some)
            .map_err(|e| invalid(e.to_string()))
    }

    /// 组装响应：`body` / `body_file` / `json` 三选一，文本里的 `{{base}}` 换成 mock server 的地址
    fn build(&self, spec: ResponseSpec, route: &str) -> Result<Built, HyErr> {
        let base = &self.shared.base;
        let mut headers: Vec<(String, String)> = spec.headers.into_iter().collect();
        let body = match (spec.body, spec.body_file, spec.json) {
            (Some(text), None, None) => text.replace("{{base}}", base).into_bytes(),
            (None, Some(file), None) => {
                let not_found =
                    || err!(HttpMockRuntimeErr::FileNotFound, { "route": route, "file": &file });
                let key = super::file_key(&file).map_err(|_| not_found())?;
                let bytes = self
                    .shared
                    .files
                    .lock()
                    .get(&key)
                    .cloned()
                    .ok_or_else(not_found)?;
                match String::from_utf8(bytes) {
                    Ok(text) => text.replace("{{base}}", base).into_bytes(),
                    Err(e) => e.into_bytes(),
                }
            }
            (None, None, Some(json)) => {
                if !headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    headers.push(("content-type".into(), "application/json".into()));
                }
                json.to_string().into_bytes()
            }
            (None, None, None) => Vec::new(),
            _ => {
                return Err(err!(HttpMockRuntimeErr::InvalidResponse, {
                    "route": route,
                    "cause": "only one of body / body_file / json is allowed",
                }));
            }
        };
        Ok((spec.status, headers, body))
    }
}
