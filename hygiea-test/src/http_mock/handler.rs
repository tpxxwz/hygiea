//! 一条 interaction 收到请求后怎么响应：按次数取固定响应，或者跑 Rhai 脚本；组装成 httpmock 的响应；
//! 出了问题不 panic（mock server 里 panic 只会让客户端断开），记进 problems，
//! 脚本出错、读文件失败时返回 500

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use httpmock::prelude::{HttpMockRequest, HttpMockResponse};
use parking_lot::Mutex;
use rhai::{AST, Dynamic, Engine, Scope};

use super::script::Req;
use super::spec::ResponseSpec;
use super::template::PathTemplate;

/// 组装好的响应：状态码、响应头、body
type Built = (u16, Vec<(String, String)>, Vec<u8>);

pub(crate) enum Responder {
    /// 固定响应就是只有一个的序列；次数超过个数后一直返回最后一个
    Sequence(Vec<ResponseSpec>),
    Script(AST),
}

/// 同一个 server 上所有 interaction 共用的东西
pub(crate) struct Shared {
    pub state: Mutex<Dynamic>,
    pub problems: Mutex<Vec<String>>,
    pub base: String,
}

impl Shared {
    fn problem(&self, message: String) {
        self.problems.lock().push(message);
    }
}

pub(crate) struct Handler {
    pub cassette: String,
    pub name: String,
    pub dir: String,
    pub template: Option<PathTemplate>,
    pub responder: Responder,
    pub match_script: Option<AST>,
    pub engine: Arc<Engine>,
    pub shared: Arc<Shared>,
    pub calls: AtomicUsize,
}

impl Handler {
    fn label(&self, calls: usize) -> String {
        format!("{} [{}] call #{calls}", self.cassette, self.name)
    }

    /// `match_script`：返回 true 才算匹配；出错按不匹配处理，并记一条问题
    pub fn matches(&self, request: &HttpMockRequest) -> bool {
        let Some(ast) = &self.match_script else {
            return true;
        };
        let req = Req::new(request, self.template.as_ref());
        let state = self.shared.state.lock().clone();
        let mut scope = Scope::new();
        scope.push("request", req);
        scope.push_dynamic("state", state);
        scope.push("base", self.shared.base.clone());
        match self.engine.eval_ast_with_scope::<bool>(&mut scope, ast) {
            Ok(matched) => matched,
            Err(e) => {
                self.shared.problem(format!(
                    "{} [{}] match_script: {e}",
                    self.cassette, self.name
                ));
                false
            }
        }
    }

    pub fn respond(&self, request: &HttpMockRequest) -> HttpMockResponse {
        let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let req = Req::new(request, self.template.as_ref());
        let built = self
            .spec(req.clone(), calls)
            .and_then(|spec| self.build(spec));
        let (status, headers, body) = match built {
            Ok(built) => built,
            Err(e) => {
                let message = format!("{}: {e}", self.label(calls));
                self.shared.problem(message.clone());
                return HttpMockResponse::builder()
                    .status(500)
                    .header("content-type", "text/plain; charset=utf-8")
                    .body(format!("hygiea-test http_mock: {message}"))
                    .build();
            }
        };
        let mut builder = HttpMockResponse::builder().status(status);
        for (k, v) in headers {
            builder = builder.header(k, v);
        }
        builder.body(body).build()
    }

    fn spec(&self, req: Req, calls: usize) -> Result<ResponseSpec, String> {
        match &self.responder {
            Responder::Sequence(list) => list
                .get((calls - 1).min(list.len().saturating_sub(1)))
                .cloned()
                .ok_or_else(|| "empty responses".to_string()),
            Responder::Script(ast) => self.run_script(ast, req, calls),
        }
    }

    /// 跑脚本：state 放进 scope 给脚本改，跑完再取回来存上
    fn run_script(&self, ast: &AST, req: Req, calls: usize) -> Result<ResponseSpec, String> {
        let mut state = self.shared.state.lock();
        let mut scope = Scope::new();
        scope.push("request", req);
        scope.push("calls", calls as i64);
        scope.push("base", self.shared.base.clone());
        scope.push_dynamic("state", state.clone());
        let result: Dynamic = self
            .engine
            .eval_ast_with_scope(&mut scope, ast)
            .map_err(|e| format!("script: {e}"))?;
        if let Some(new_state) = scope.get_value::<Dynamic>("state") {
            *state = new_state;
        }
        rhai::serde::from_dynamic(&result).map_err(|e| format!("script result: {e}"))
    }

    /// 组装响应：`body` / `body_file` / `json` 三选一，文本里的 `{{base}}` 换成 mock server 的地址
    fn build(&self, spec: ResponseSpec) -> Result<Built, String> {
        let base = &self.shared.base;
        let mut headers: Vec<(String, String)> = spec.headers.into_iter().collect();
        let body = match (spec.body, spec.body_file, spec.json) {
            (Some(text), None, None) => text.replace("{{base}}", base).into_bytes(),
            (None, Some(file), None) => {
                let path = format!("{}/{file}", self.dir);
                let bytes = std::fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
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
            _ => return Err("only one of body / body_file / json is allowed".into()),
        };
        Ok((spec.status, headers, body))
    }
}
