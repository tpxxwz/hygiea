//! http_mock 的集成测试，用 `tests/resources/httpmock/` 下的示例服务：
//! shop/ 是一个小商店（登录、商品列表、商品详情、下载 / 上传文件，外加几个演示用的接口），
//! conflict/、bad_state/、bad_handler/、bad_response/、bad_script/ 是启动就该失败的服务。客户端用原生 reqwest。跑法：
//! cargo test -p hygiea-test --features http-mock --test http_mock

use std::time::{Duration, Instant};

use hygiea_test::cassette_root;
use hygiea_test::http_mock::{
    Cassette, Handler, HttpMockErr, HttpMockRuntimeErr, Mocked, PROBLEM_HEADER, Response,
};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

async fn shop() -> Mocked {
    Cassette::load(cassette_root!(), "shop")
        .start()
        .await
        .unwrap()
}

async fn login(client: &Client, mock: &Mocked, name: &str, password: &str) -> reqwest::Response {
    client
        .post(mock.url("/api/login"))
        .json(&json!({ "name": name, "password": password }))
        .send()
        .await
        .unwrap()
}

#[derive(Deserialize)]
struct Db {
    token_seq: i64,
    sessions: serde_json::Map<String, Value>,
}

#[tokio::test]
async fn full_flow() {
    let mock = shop().await;
    let client = Client::new();
    let get = |path: &str, token: &str| client.get(mock.url(path)).header("x-token", token).send();

    // 没登录
    let resp = get("/api/items", "none").await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 密码错误 / 正确
    let bad = login(&client, &mock, "alice", "x").await;
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
    let token = login(&client, &mock, "alice", "pw-alice")
        .await
        .json::<Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    // 列表：过滤、分页
    let page: Value = client
        .get(mock.url("/api/items"))
        .query(&[("q", "ap"), ("offset", "1"), ("limit", "1")])
        .header("x-token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["total"], 2); // apple、apricot
    assert_eq!(page["items"][0]["name"], "apricot");

    // 详情：路径变量；没有的 404
    let item: Value = get("/api/items/i3", &token)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(item["price"], 9);
    assert_eq!(
        get("/api/items/nope", &token).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );

    // 下载：响应体里的 {{base}} 换成了 mock 地址
    let text = get("/api/files/hello.txt", &token)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains(&format!("{}/api/files/next.txt", mock.base_url())));

    // 服务端让会话过期
    mock.update_state(|db| db["sessions"] = json!({})).unwrap();
    assert_eq!(
        get("/api/items", &token).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let db: Db = mock.state().unwrap();
    assert_eq!(db.token_seq, 1);
    assert!(db.sessions.is_empty());
    mock.assert_valid();
}

/// /api/items/featured 是字面量，比 /api/items/{id} 优先
#[tokio::test]
async fn literal_route_wins_over_template() {
    let mock = shop().await;
    let client = Client::new();

    let featured: Value = client
        .get(mock.url("/api/items/featured"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(featured["items"][0]["id"], "i3");
    // 其他 id 还是走商品详情（没带 token，401）
    let resp = client.get(mock.url("/api/items/i1")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    mock.assert_valid();
}

/// 特殊数据不另写 db，测试里改 state
#[tokio::test]
async fn scenario_by_update_state() {
    let mock = shop().await;
    mock.update_state(|db| db["sessions"]["tk-pre"] = json!({ "user_id": 1 }))
        .unwrap();
    let client = Client::new();
    let get = |token: &'static str| {
        client
            .get(mock.url("/api/items/i2"))
            .header("x-token", token)
            .send()
    };

    let item: Value = get("tk-pre").await.unwrap().json().await.unwrap();
    assert_eq!(item["name"], "banana");
    assert_eq!(
        get("tk-other").await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    mock.assert_valid();
}

#[tokio::test]
async fn flaky_responses() {
    let mock = shop().await;
    let client = Client::new();

    // delay_ms
    let started = Instant::now();
    client.get(mock.url("/api/slow")).send().await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(300));

    // match_script：有 x-beta 头才接得住
    let beta: Value = client
        .get(mock.url("/api/beta"))
        .header("x-beta", "1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(beta["next_cursor"], "c1");
    mock.assert_valid();

    // 没有 x-beta 头：没有路由，404，记一条问题
    let resp = client.get(mock.url("/api/beta")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let problems = mock.take_problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with("No route: GET /api/beta: method and path match 灰度 (routes.toml), but its other conditions"),
        "{problems:?}"
    );
}

#[tokio::test]
async fn request_hooks() {
    let mock = shop().await;
    let client = Client::new();
    let url = mock.url("/echo/42?q=hello&tag=a&tag=b");

    let json: Value = client
        .post(&url)
        .header("x-token", "abc")
        .header("accept", "text/plain")
        .header("accept", "application/json")
        .header("cookie", "a=1; sid=s-9; tag=x")
        .header("cookie", "tag=y")
        .json(&json!({ "user": { "id": 7 } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(json["method"], "POST");
    assert_eq!(json["id"], "42");
    assert_eq!(json["q"], "hello");
    assert_eq!(json["tags"], json!(["a", "b"]));
    assert_eq!(
        json["queries_all"],
        json!({ "q": ["hello"], "tag": ["a", "b"] })
    );
    assert_eq!(
        json["accept_all"],
        json!(["text/plain", "application/json"])
    );
    assert_eq!(json["headers_all_accept"], json["accept_all"]);
    assert_eq!(json["missing"], Value::Null);
    assert_eq!(json["token"], "abc");
    assert_eq!(json["sid"], "s-9");
    assert_eq!(
        json["cookies"],
        json!({ "a": "1", "sid": "s-9", "tag": "x" })
    );
    assert_eq!(
        json["cookies_all"],
        json!({ "a": ["1"], "sid": ["s-9"], "tag": ["x", "y"] })
    );
    assert_eq!(json["tag_cookies"], json!(["x", "y"]));
    assert_eq!(json["body"]["user"]["id"], 7);
    assert_eq!(json["base"], mock.base_url());

    let form: Value = client
        .post(&url)
        .form(&[("name", "猫"), ("n", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(form["body"], json!({ "name": "猫", "n": "1" }));

    let (gbk, _, _) = encoding_rs::GBK.encode("猫");
    let text: Value = client
        .post(&url)
        .header("content-type", "text/plain; charset=gbk")
        .body(gbk.into_owned())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(text["body"], "猫");
    assert_eq!(text["body_len"], 2);
    mock.assert_valid();
}

/// 上传的文件只进这个 server 的内存文件表：能下载回来，测试里能查，别的 server 看不到，也不落盘
#[tokio::test]
async fn uploaded_file_stays_in_memory() {
    let mock = shop().await;
    let other = shop().await;
    let client = Client::new();
    let bytes = vec![0xff_u8, 0xfb, 0x00, 0x01];

    let resp = client
        .put(mock.url("/files/a.bin"))
        .body(bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = client.get(mock.url("/files/a.bin")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.bytes().await.unwrap().to_vec(), bytes);
    assert_eq!(mock.file("uploads/a.bin"), Some(bytes));
    // 启动时读进来的 data/static 也在表里
    assert!(mock.file("hello.txt").is_some());
    assert!(
        !std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/resources/httpmock/shop/data/static/uploads"
        ))
        .exists()
    );
    mock.assert_valid();

    // 另一个 server 没有这个文件：body_file 找不到，记一条问题，返回 500
    let resp = client.get(other.url("/files/a.bin")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(other.has_problem(HttpMockRuntimeErr::FileNotFound));
    let problems = other.take_problems();
    assert!(
        problems[0]
            .contains("File not found: routes.toml [下载] GET /files/a.bin call #1: uploads/a.bin"),
        "{problems:?}"
    );
}

/// 测试里换掉文件：只影响这个 server
#[tokio::test]
async fn set_file_replaces_static_file() {
    let mock = shop().await;
    mock.set_file("hello.txt", "changed");
    let text = Client::new()
        .get(mock.url("/api/files/x"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(text, "changed");
    mock.assert_valid();
}

/// handler 链：第一步校验，返回 () 交给下一步；第二步前等 100ms
#[tokio::test]
async fn handler_chain() {
    let mock = shop().await;
    let client = Client::new();

    let denied = client.get(mock.url("/api/admin")).send().await.unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let started = Instant::now();
    let ok: Value = client
        .get(mock.url("/api/admin"))
        .header("x-token", "admin")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ok["ok"], true);
    assert!(started.elapsed() >= Duration::from_millis(100));
    mock.assert_valid();

    // 整条链都没给出响应
    let resp = client.get(mock.url("/api/nothing")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let problems = mock.take_problems();
    assert!(
        problems[0].contains(
            "No response: routes.toml [没有响应] GET /api/nothing call #1: every handler returned () [#1 script pass.rhai]"
        ),
        "{problems:?}"
    );
}

/// 同样的请求、没有状态变化，结果却不一样：脚本里用 calls
#[tokio::test]
async fn calls_for_stateless_flaky_upstream() {
    let mock = shop().await;
    let client = Client::new();
    let mut statuses = Vec::new();
    for _ in 0..3 {
        statuses.push(
            client
                .get(mock.url("/api/unstable"))
                .send()
                .await
                .unwrap()
                .status(),
        );
    }
    assert_eq!(
        statuses,
        [
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::OK
        ]
    );
    mock.assert_valid();

    // 第 4 次没定义：500，记一条问题
    let resp = client.get(mock.url("/api/unstable")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(mock.has_problem(HttpMockRuntimeErr::ScriptFailed));
    let problems = mock.take_problems();
    assert!(
        problems[0].starts_with(
            "Script failed: routes.toml [不稳定] GET /api/unstable call #4, handler #1: "
        ),
        "{problems:?}"
    );
    assert!(
        problems[0].contains("nth: no item #4, the list has 3 item(s)"),
        "{problems:?}"
    );
}

// ============================================================
// 用代码定义
// ============================================================

/// 全用代码：state、文件、路由（行内脚本、固定响应、链）
#[tokio::test]
async fn code_only_service() {
    let mock = Cassette::new()
        .state(json!({ "greeting": "hello" }))
        .file("page.html", "<a href=\"{{base}}/next\">next</a>")
        .route(
            "GET",
            "/hello/{name}",
            Handler::script_str(
                r#"#{ json: #{ text: `${state.greeting}, ${request.param("name")}` } }"#,
            ),
        )
        .route(
            "GET",
            "/page",
            Handler::response(Response::file("page.html").header("content-type", "text/html")),
        )
        .route_chain(
            "GET",
            "/down",
            [
                Handler::script_str("()"),
                Handler::response(Response::text("bad gateway").status(502)).delay_ms(50),
            ],
        )
        .start()
        .await
        .unwrap();
    let client = Client::new();

    let hello: Value = client
        .get(mock.url("/hello/cat"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hello["text"], "hello, cat");

    let page = client.get(mock.url("/page")).send().await.unwrap();
    assert_eq!(page.headers()["content-type"], "text/html");
    assert!(page.text().await.unwrap().contains(&mock.base_url()));

    let started = Instant::now();
    let down = client.get(mock.url("/down")).send().await.unwrap();
    assert_eq!(down.status(), StatusCode::BAD_GATEWAY);
    assert!(started.elapsed() >= Duration::from_millis(50));
    mock.assert_valid();
}

/// 脚本里的 DateTime：放进 state 请求之间还是对象，能比较；转 JSON 是 UTC 的 RFC 3339
#[tokio::test]
async fn datetime_in_state() {
    let mock = Cassette::new()
        .state(json!({ "expire_at": "2026-01-01T09:00:00+09:00" }))
        .route(
            "POST",
            "/renew",
            Handler::script_str(
                r#"state.expire_at = parse_rfc3339(state.expire_at).shift_days(5); #{ json: #{ at: state.expire_at } }"#,
            ),
        )
        .route(
            "GET",
            "/check",
            Handler::script_str(
                r#"let e = state.expire_at;
                #{ json: #{
                    after: e > from_secs(e.secs - 1) && e >= e && e == parse_rfc3339(e.to_rfc3339()),
                    expired: e < now_utc(),
                    millis: e.millis,
                    text: `${e}`,
                } }"#,
            ),
        )
        .route("GET", "/bad", Handler::script_str(r#"parse_rfc3339("x")"#))
        .start()
        .await
        .unwrap();
    let client = Client::new();

    let renew: Value = client
        .post(mock.url("/renew"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(renew["at"], "2026-01-06T00:00:00Z");
    let check: Value = client
        .get(mock.url("/check"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(check["after"], true);
    assert_eq!(check["expired"], true);
    assert_eq!(check["millis"], 1_767_657_600_000_i64);
    assert_eq!(check["text"], "2026-01-06T00:00:00Z");
    assert_eq!(
        mock.state::<Value>().unwrap()["expire_at"],
        "2026-01-06T00:00:00Z"
    );

    let bad = client.get(mock.url("/bad")).send().await.unwrap();
    assert_eq!(bad.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(mock.take_problems().len(), 1);
    mock.assert_valid();
}

/// data_suffix：同一套路由和脚本，state 和 static 换成 data_alt 下的
#[tokio::test]
async fn data_suffix_uses_other_data_dir() {
    let mock = Cassette::load(cassette_root!(), "shop")
        .data_suffix("alt")
        .route(
            "GET",
            "/probe",
            Handler::script_str(
                r#"#{ json: #{ marker: state.marker, file: read_text("marker.txt") } }"#,
            ),
        )
        .start()
        .await
        .unwrap();
    let probe: Value = Client::new()
        .get(mock.url("/probe"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(probe, json!({ "marker": "alt", "file": "alt file" }));
    // data/ 下的 state 没读进来
    assert!(mock.state::<Value>().unwrap().get("users").is_none());
    mock.assert_valid();
}

/// 目录 + 代码补充：state 整个替换、文件覆盖、多一条路由，和目录里的路由一起检查冲突
#[tokio::test]
async fn dir_with_code_overrides() {
    let mock = Cassette::load(cassette_root!(), "shop")
        .state(json!({ "token_seq": 0, "users": [], "sessions": { "tk": { "user_id": 1 } }, "items": [] }))
        .file("hello.txt", "replaced")
        .route("GET", "/api/extra", Handler::script("item.rhai"))
        .start()
        .await
        .unwrap();
    let client = Client::new();

    let items: Value = client
        .get(mock.url("/api/items"))
        .header("x-token", "tk")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(items["total"], 0);
    let text = client
        .get(mock.url("/api/files/x"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(text, "replaced");
    // 目录 rhai/ 下的脚本，代码路由也能用
    let extra = client
        .get(mock.url("/api/extra"))
        .header("x-token", "tk")
        .send()
        .await
        .unwrap();
    assert_eq!(extra.status(), StatusCode::NOT_FOUND);
    mock.assert_valid();

    // 和目录里的 GET /api/items/{id} 冲突
    let conflict = Cassette::load(cassette_root!(), "shop")
        .route(
            "GET",
            "/api/items/{sku}",
            Handler::response(Response::empty()),
        )
        .start()
        .await
        .err()
        .unwrap();
    assert!(conflict.is(HttpMockErr::RouteConflict), "{conflict}");
}

/// 固定响应的 body_file 启动时就查
#[tokio::test]
async fn fixed_body_file_checked_at_start() {
    let err = Cassette::new()
        .route("GET", "/x", Handler::response(Response::file("nope.html")))
        .start()
        .await
        .err()
        .unwrap();
    assert!(err.is(HttpMockErr::FileNotFound), "{err}");
    assert_eq!(
        err.to_string(),
        "File not found: (code) [GET /x] handler #1: nope.html (relative to data/static/)"
    );
}

#[tokio::test]
async fn code_only_rejects_script_file() {
    let err = Cassette::new()
        .route("GET", "/x", Handler::script("x.rhai"))
        .start()
        .await
        .err()
        .unwrap();
    assert!(err.is(HttpMockErr::InvalidCassette), "{err}");
    assert!(err.to_string().contains("script_str"), "{err}");

    let err = Cassette::new()
        .state(json!([1]))
        .start()
        .await
        .err()
        .unwrap();
    assert!(err.is(HttpMockErr::InvalidState), "{err}");
}

// ============================================================
// 问题和加载错误
// ============================================================

#[tokio::test]
async fn script_error_is_reported() {
    let mock = shop().await;
    let resp = Client::new().get(mock.url("/broken")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let problems = mock.take_problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0]
            .starts_with("Script failed: routes.toml [脚本出错] GET /broken call #1, handler #1: "),
        "{problems:?}"
    );
}

/// 死循环超过执行步数上限：中断，ScriptFailed，测试不会卡住
#[tokio::test]
async fn infinite_loop_is_stopped() {
    let mock = shop().await;
    let resp = Client::new()
        .get(mock.url("/api/forever"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(mock.has_problem(HttpMockRuntimeErr::ScriptFailed));
    let problems = mock.take_problems();
    assert!(
        problems[0].to_lowercase().contains("too many operations"),
        "{problems:?}"
    );
}

/// 有问题但没处理（没 assert_valid、没 take_problems）：mock drop 时 panic，问题不会被吞掉
#[tokio::test]
#[should_panic(expected = "not checked before the mock was dropped")]
async fn unchecked_problems_panic_on_drop() {
    let mock = shop().await;
    let resp = Client::new().get(mock.url("/nope")).send().await.unwrap();
    // client 拿到 404 就当普通错误处理了，测试本身不知道是 mock 没配这个路由
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unmatched_request_is_reported() {
    let mock = shop().await;
    let resp = Client::new()
        .get(mock.url("/nope?x=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // 错误作为响应返回：带上错误码响应头，body 是 {"err_code","message"}
    let code = resp.headers()[PROBLEM_HEADER].to_str().unwrap().to_string();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["err_code"], code);
    assert_eq!(
        body["message"],
        "No route: GET /nope?x=1: no route's path matches /nope"
    );

    // 只有别的 method
    let resp = Client::new()
        .delete(mock.url("/api/items/i1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(mock.has_problem(HttpMockRuntimeErr::NoRoute));
    assert_eq!(
        mock.take_problems(),
        [
            "No route: GET /nope?x=1: no route's path matches /nope",
            "No route: DELETE /api/items/i1: path /api/items/i1 only has routes for GET",
        ]
    );
}

#[tokio::test]
async fn load_errors() {
    let start = |dir: &'static str| async move {
        Cassette::load(cassette_root!(), dir)
            .start()
            .await
            .err()
            .unwrap()
    };

    let missing = start("nope").await;
    assert!(missing.is(HttpMockErr::CassetteNotFound), "{missing}");

    let empty_chain = start("bad_handler").await;
    assert!(
        empty_chain.is(HttpMockErr::InvalidCassette),
        "{empty_chain}"
    );
    assert!(
        empty_chain.to_string().contains("at least one handler"),
        "{empty_chain}"
    );

    let two_bodies = start("bad_response").await;
    assert!(two_bodies.is(HttpMockErr::InvalidCassette), "{two_bodies}");
    assert!(
        two_bodies
            .to_string()
            .contains("[两个 body] handler #1: only one of body / body_file / json is allowed"),
        "{two_bodies}"
    );

    // 严格变量：用了没定义的变量，编译时就报错
    let typo = start("bad_script").await;
    assert!(typo.is(HttpMockErr::ScriptCompileFailed), "{typo}");
    assert!(format!("{typo:#}").contains("stat"), "{typo:#}");

    let not_object = start("bad_state").await;
    assert!(not_object.is(HttpMockErr::InvalidState), "{not_object}");

    // GET /a/{x} 和 GET /a/{id:[0-9]+} 冲突；DELETE 的不算
    let conflict = start("conflict").await;
    assert!(conflict.is(HttpMockErr::RouteConflict), "{conflict}");
    let message = conflict.to_string();
    assert!(message.contains("GET /a/{x}"), "{message}");
    assert!(message.contains("GET /a/{id:[0-9]+}"), "{message}");
    assert!(message.contains("e.g. /a/0"), "{message}");
}
