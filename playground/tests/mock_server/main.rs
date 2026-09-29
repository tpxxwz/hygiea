//! httpmock 用法速查：本地起 mock HTTP 服务，客户端用原生 reqwest 调。数据都是编的。
//! 不联网，直接跑：
//! cd playground && cargo test --test mock_server
//!
//! 分三部分：
//! - 手写 mock（墨墨记忆卡接口）：`when` 写请求匹配条件，`then` 写响应；没匹配上的请求返回 404；
//!   `mock.assert_async()` 断言恰好被调了一次，`assert_calls_async(n)` 断言 n 次
//! - 完整模拟服务：`<项目>/server/` 下的接口一起挂到一个 server，共用一份 state
//!   （相当于后端数据库，初始值是 `db.toml` 的 `state_file`），脚本模拟真实服务的逻辑，走完整流程
//! - 单接口用例：`<项目>/cases/` 下每个文件自己一个 server，按请求参数、请求体、
//!   调用次数返回不同结果；也可以有自己的 state
//!
//! cassette 支持 httpmock 原生 YAML 和自定义 TOML，见 `cassette.rs`。目录 `tests/resources/httpmock/`
//! 按项目分（`moji/`、`momo/markji/`，通用的放 `common/`），每个项目下：
//! - `server/`：接口的 cassette（`.toml`）和脚本（`.rhai`）放在一起，`common.rhai` 放共用函数；
//!   `data/` 放 state 初始数据和只读数据（词库），`bodies/` 放响应体（html / js / mp3）
//! - `cases/`：单接口用例的 cassette 和脚本
//!
//! state 一个 server 一份，每个测试起自己的 server，测试之间互不影响

mod cassette;

use httpmock::prelude::*;
use reqwest::{Client, StatusCode, multipart};
use serde::Deserialize;
use serde_json::{Value, json};

/// 真实地址是 https://open.maimemo.com/open/api/v1/markji，mock 里只保留路径部分
const API: &str = "/open/api/v1/markji";
const TOKEN: &str = "test-token";

fn deck(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "source": "SELF",
        "status": "NORMAL",
        "name": name,
        "description": "",
        "creator": "u_1",
        "authors": ["u_1"],
        "revision": 3,
        "is_private": true,
        "card_count": 2,
        "chapter_count": 1,
        "created_time": "2026-01-01T00:00:00.000Z",
        "updated_time": "2026-01-02T00:00:00.000Z"
    })
}

fn card(id: &str, content: &str) -> Value {
    json!({
        "id": id,
        "status": "NORMAL",
        "deck_id": "d_1",
        "revision": 1,
        "content": content,
        "content_type": "PLAIN",
        "files": [],
        "creator": "u_1",
        "source": "SELF",
        "grammar_version": 3,
        "created_time": "2026-01-01T00:00:00.000Z",
        "updated_time": "2026-01-01T00:00:00.000Z"
    })
}

/// 成功响应的外层：`{"success":true,"data":{...},"errors":[]}`
fn ok(data: Value) -> Value {
    json!({ "success": true, "data": data, "errors": [] })
}

#[tokio::test]
async fn get_with_bearer_and_query() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(GET)
                .path(format!("{API}/decks"))
                .query_param("offset", "0")
                .query_param("limit", "10")
                .header("authorization", format!("Bearer {TOKEN}"));
            then.status(200).json_body(ok(json!({
                "decks": [deck("d_1", "N2 单词"), deck("d_2", "N1 语法")],
                "total": 2
            })));
        })
        .await;

    let body: Value = Client::new()
        .get(server.url(format!("{API}/decks")))
        .query(&[("offset", "0"), ("limit", "10")])
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    mock.assert_async().await;
    assert_eq!(body["data"]["total"], 2);
    assert_eq!(body["data"]["decks"][1]["name"], "N1 语法");
}

#[tokio::test]
async fn unmatched_request_gets_404() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(GET)
                .path(format!("{API}/decks/folders"))
                .header("authorization", format!("Bearer {TOKEN}"));
            then.status(200).json_body(ok(json!({ "folders": [] })));
        })
        .await;

    // 没带 token，匹配不上
    let resp = Client::new()
        .get(server.url(format!("{API}/decks/folders")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    mock.assert_calls_async(0).await;
}

#[tokio::test]
async fn error_envelope() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path(format!("{API}/decks/folders"));
            then.status(401).json_body(json!({
                "errors": [{ "code": "common_unauthorized", "msg": "Authorization failed", "info": "" }],
                "success": false
            }));
        })
        .await;

    let resp = Client::new()
        .get(server.url(format!("{API}/decks/folders")))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["success"], false);
    assert_eq!(body["errors"][0]["code"], "common_unauthorized");
}

#[tokio::test]
async fn post_json_exact_and_partial_match() {
    let server = MockServer::start_async().await;
    // 新建卡片：请求体完全相等才匹配
    let create = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(format!("{API}/decks/d_1/chapters/c_1/cards"))
                .json_body(json!({
                    "deck": "d_1",
                    "chapter": "c_1",
                    "card": { "content": "日本語\n---\nにほんご", "grammar_version": 3 }
                }));
            then.status(200).json_body(ok(json!({
                "card": card("k_9", "日本語\n---\nにほんご"),
                "chapter": {
                    "id": "c_1", "deck_id": "d_1", "name": "第一章", "revision": 2,
                    "card_ids": ["k_1", "k_9"], "creator": "u_1",
                    "created_time": "2026-01-01T00:00:00.000Z",
                    "updated_time": "2026-01-03T00:00:00.000Z"
                }
            })));
        })
        .await;
    // 修改章节名：只要求请求体里包含这些字段，revision 等其他字段随意
    let rename = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(format!("{API}/decks/d_1/chapters/c_1"))
                .json_body_includes(r#"{ "name": "第二章" }"#);
            then.status(200)
                .json_body(ok(json!({ "chapter": { "id": "c_1", "name": "第二章" } })));
        })
        .await;

    let client = Client::new();
    let created: Value = client
        .post(server.url(format!("{API}/decks/d_1/chapters/c_1/cards")))
        .json(&json!({
            "deck": "d_1",
            "chapter": "c_1",
            "card": { "content": "日本語\n---\nにほんご", "grammar_version": 3 }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let renamed = client
        .post(server.url(format!("{API}/decks/d_1/chapters/c_1")))
        .json(&json!({ "deck": "d_1", "chapter": "c_1", "name": "第二章", "revision": 7 }))
        .send()
        .await
        .unwrap();

    create.assert_async().await;
    rename.assert_async().await;
    assert_eq!(created["data"]["card"]["id"], "k_9");
    assert_eq!(created["data"]["chapter"]["card_ids"][1], "k_9");
    assert_eq!(renamed.status(), StatusCode::OK);
}

#[tokio::test]
async fn multipart_upload() {
    let server = MockServer::start_async().await;
    // multipart 没有专门的匹配器，按 body 里的片段匹配
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(format!("{API}/files"))
                .header_prefix("content-type", "multipart/form-data")
                .body_includes(r#"name="file"; filename="word.mp3""#)
                .body_includes(r#"name="deck_id""#);
            then.status(200).json_body(ok(json!({
                "file": {
                    "id": "f_1",
                    "url": "https://example.com/f_1.mp3",
                    "mime": "audio/mpeg",
                    "size": 4,
                    "info": {},
                    "expire_time": "2026-02-01T00:00:00.000Z"
                }
            })));
        })
        .await;

    let form = multipart::Form::new().text("deck_id", "d_1").part(
        "file",
        multipart::Part::bytes(b"ID3fake".to_vec())
            .file_name("word.mp3")
            .mime_str("audio/mpeg")
            .unwrap(),
    );
    let body: Value = Client::new()
        .post(server.url(format!("{API}/files")))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    mock.assert_async().await;
    assert_eq!(body["data"]["file"]["id"], "f_1");
}

#[tokio::test]
async fn call_count_and_delete_mock() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path(format!("{API}/decks/d_1/cards/k_1"));
            then.status(200)
                .json_body(ok(json!({ "card": card("k_1", "猫\n---\nねこ") })));
        })
        .await;

    let client = Client::new();
    for _ in 0..3 {
        let resp = client
            .get(server.url(format!("{API}/decks/d_1/cards/k_1")))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
    mock.assert_calls_async(3).await;

    // 删掉以后同样的请求就是 404
    mock.delete_async().await;
    let resp = client
        .get(server.url(format!("{API}/decks/d_1/cards/k_1")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ============================================================
// cassette：完整模拟服务（<项目>/server/，一起挂到一个 server）
// ============================================================

const LOGIN: &str = "/app/mojidict/api/v1/account/unifiedLogin";
const SEARCH: &str = "/app/mojidict/api/v2/search/all";

/// moji 模拟服务的数据库，和 moji/server/data/db.json 对应
#[derive(Deserialize)]
struct MojiDb {
    token_seq: i64,
    sessions: serde_json::Map<String, Value>,
}

/// 登录 → 搜索 → 会话过期 → 重新登录 → 搜索 → 取发音 → 下载，全部走同一个 server 和同一份 state
#[tokio::test]
async fn moji_server_full_flow() {
    let mock = &cassette::start_all(&[
        "moji/server/db",
        "moji/server/login",
        "moji/server/keyword_search",
        "moji/server/tts_fetch",
        "moji/server/tts_download",
    ])
    .await;
    let client = &Client::new();
    let login = |password: &'static str| async move {
        client
            .post(mock.url(LOGIN))
            .json(&json!({
                "authName": "PasswordAuth",
                "authPayload": { "email": "neko@example.com", "code": password }
            }))
            .send()
            .await
            .unwrap()
    };
    let search = |token: Option<String>| async move {
        let mut req = client.get(mock.url(SEARCH)).query(&[("text", "ねこ")]);
        if let Some(token) = token {
            req = req.header("x-moji-token", token);
        }
        req.send().await.unwrap()
    };
    let token_of = |body: Value| body["sessionToken"].as_str().unwrap().to_string();

    // 没登录就搜索：会话过期
    assert_eq!(search(None).await.status(), StatusCode::BAD_REQUEST);

    // 密码错误
    let wrong = login("bad").await;
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    assert_eq!(wrong.json::<Value>().await.unwrap()["code"], 100000003);

    // 登录成功，脚本把会话写进 state.sessions，搜索就能过
    let t1 = token_of(login("nyan123").await.json().await.unwrap());
    let found: Value = search(Some(t1.clone())).await.json().await.unwrap();
    assert_eq!(found["word"]["list"].as_array().unwrap().len(), 3);

    // 模拟服务端让会话过期：直接清掉 state 里的会话
    mock.update_state(|db| db["sessions"] = json!({}));
    let expired = search(Some(t1.clone())).await;
    assert_eq!(expired.status(), StatusCode::BAD_REQUEST);
    assert_eq!(expired.json::<Value>().await.unwrap()["code"], 100000022);

    // 重新登录拿到新 token，搜索恢复
    let t2 = token_of(login("nyan123").await.json().await.unwrap());
    assert_ne!(t1, t2);
    assert_eq!(search(Some(t2.clone())).await.status(), StatusCode::OK);

    // 发音：取地址（要带 token）再下载，下载地址由 tts_fetch 签发
    let tts: Value = client
        .get(mock.url("/app/mojidict/api/v1/tts/fetch"))
        .query(&[("targetId", "198951468"), ("targetType", "102")])
        .header("x-moji-token", t2.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mp3 = client
        .get(tts["url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(&mp3[..3], b"ID3");

    // 状态里只剩第二次登录的会话
    let db: MojiDb = mock.state();
    assert_eq!(db.token_seq, 2);
    assert_eq!(db.sessions.keys().collect::<Vec<_>>(), [&t2]);
}

/// tts_fetch 签发下载地址、tts_download 只认签发过的地址，两个接口靠 state.tts_links 联动。
/// 没挂 db，跳过 token 校验
#[tokio::test]
async fn moji_server_tts_fetch_and_download() {
    let mock = &cassette::start_all(&["moji/server/tts_fetch", "moji/server/tts_download"]).await;
    let client = &Client::new();
    let fetch = |id: &'static str, voice: Option<&'static str>| async move {
        let mut req = client
            .get(mock.url("/app/mojidict/api/v1/tts/fetch"))
            .query(&[("targetId", id), ("targetType", "102")]);
        if let Some(voice) = voice {
            req = req.query(&[("voiceId", voice)]);
        }
        req.send().await.unwrap()
    };

    // 取地址：按 targetId 查发音表，签发带 sign 的地址
    let neko: Value = fetch("198951468", None).await.json().await.unwrap();
    assert_eq!(neko["text"], "猫");
    let neko_url = neko["url"].as_str().unwrap().to_string();
    assert_eq!(
        neko_url,
        format!("{}/tts/f002/198951468.mp3?sign=s1", mock.base_url())
    );
    let nekojita: Value = fetch("198951470", Some("m001")).await.json().await.unwrap();
    let nekojita_url = nekojita["url"].as_str().unwrap().to_string();
    assert!(nekojita_url.contains("/tts/m001/198951470.mp3?sign=s2"));

    // 表里没有的返回 404
    assert_eq!(fetch("1", None).await.status(), StatusCode::NOT_FOUND);

    // 签发过的地址能下载
    let resp = client.get(&neko_url).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "audio/mpeg");
    assert_eq!(&resp.bytes().await.unwrap()[..3], b"ID3");

    // 路径对不上 /tts/{voice}/{id}.mp3（多一段、扩展名不对）：没有匹配的 mock，404
    for path in ["/tts/f002/x/198951468.mp3", "/tts/f002/198951468.wav"] {
        let resp = client
            .get(format!("{}{path}?sign=s1", mock.base_url()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
    }

    // 没签发过的 sign、没带 sign、拿别的地址的 sign 下载：都是 403
    let base = mock.base_url();
    for (url, code) in [
        (
            format!("{base}/tts/f002/198951468.mp3?sign=nope"),
            "AccessDenied",
        ),
        (format!("{base}/tts/f002/198951468.mp3"), "AccessDenied"),
        (
            format!("{base}/tts/f002/198951470.mp3?sign=s1"),
            "SignatureDoesNotMatch",
        ),
    ] {
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{url}");
        assert!(resp.text().await.unwrap().contains(code), "{url}");
    }
}

/// server/ 里的接口也能单独挂：没有 db，keyword_search 跳过 token 校验，只测查询逻辑
#[tokio::test]
async fn moji_server_keyword_search_alone() {
    let mock = cassette::start("moji/server/keyword_search").await;
    let client = Client::new();
    let search = |query: &'static [(&'static str, &'static str)]| {
        client.get(mock.url(SEARCH)).query(query).send()
    };

    // 按 text 在词库里查：spell 或 pron 包含就命中
    let body: Value = search(&[("text", "猫"), ("types", "102"), ("types", "106")])
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let titles: Vec<&str> = body["word"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["猫 | ねこ ①", "猫舌 | ねこじた ⓪", "招き猫 | まねきねこ ③"]
    );

    // 按读音也能查到
    let body: Value = search(&[("text", "いぬ")])
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["word"]["list"][0]["targetId"], "198952001");

    // types 里没有单词就不查单词
    let body: Value = search(&[("text", "猫"), ("types", "106")])
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["word"]["list"].as_array().unwrap().is_empty());

    // 没传 text
    assert_eq!(search(&[]).await.unwrap().status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn moji_resource_index_and_script() {
    let mock = cassette::start("moji/server/resource_index").await;

    let client = Client::new();
    let html = client
        .get(mock.url("/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // {{base}} 已换成 mock 地址，从 html 里拿到的 js 地址直接能请求
    let script_url = format!("{}/_nuxt/app.js", mock.base_url());
    assert!(html.contains(&format!(r#"href="{script_url}" as="script""#)));

    let resp = client.get(&script_url).send().await.unwrap();
    assert_eq!(resp.headers()["content-type"], "application/javascript");
    let script = resp.text().await.unwrap();
    assert!(script.contains(r#""version":"4.13.1""#));
    assert!(script.contains(r#""X-MOJI-OS"]="PCWeb""#));
}

// ---------- 墨墨记忆卡 ----------

/// 墨墨记忆卡模拟服务：db 里有文件夹、牌组，脚本校验 Bearer token、按参数过滤分页。
/// list_folders 要排在 get_deck 前面：/decks/{deck} 也能匹配 /decks/folders，先注册的优先
#[tokio::test]
async fn markji_server_flow() {
    let mock = &cassette::start_all(&[
        "momo/markji/server/db",
        "momo/markji/server/list_folders",
        "momo/markji/server/list_decks",
        "momo/markji/server/get_deck",
    ])
    .await;
    let client = &Client::new();
    let get = |path: String, query: &'static [(&'static str, &'static str)]| async move {
        let resp = client
            .get(mock.url(&format!("{API}{path}")))
            .query(query)
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        (status, resp.json::<Value>().await.unwrap())
    };

    // token 不对：401，墨墨的错误格式
    let resp = client
        .get(mock.url(&format!("{API}/decks/folders")))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["errors"][0]["code"],
        "common_unauthorized"
    );

    // 文件夹：/decks/folders 没被 /decks/{deck} 抢走
    let (_, folders) = get("/decks/folders".into(), &[]).await;
    assert_eq!(folders["data"]["folders"][1]["parent_id"], "fd_1");

    // 牌组列表：分页，total 是总数
    let (_, page) = get("/decks".into(), &[("offset", "1"), ("limit", "1")]).await;
    assert_eq!(page["data"]["total"], 3);
    assert_eq!(page["data"]["decks"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"]["decks"][0]["id"], "d_2");

    // 按文件夹过滤
    let (_, in_folder) = get("/decks".into(), &[("folder_id", "fd_2")]).await;
    assert_eq!(in_folder["data"]["total"], 1);
    assert_eq!(in_folder["data"]["decks"][0]["name"], "N1 语法");
    let (status, _) = get("/decks".into(), &[("folder_id", "fd_x")]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 牌组信息：派生牌组 with_root=true 带上游
    let (_, fork) = get("/decks/d_2".into(), &[("with_root", "true")]).await;
    assert_eq!(
        fork["data"]["deck"]["root_deck"]["name"],
        "JLPT N1 公开牌组"
    );
    let (_, plain) = get("/decks/d_2".into(), &[]).await;
    assert!(plain["data"]["deck"].get("root_deck").is_none());
    let (status, body) = get("/decks/nope".into(), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["errors"][0]["code"], "markji_deck_not_found");
}

#[tokio::test]
async fn markji_list_decks_and_get_deck() {
    let decks_mock = cassette::start("momo/markji/cases/list_decks").await;
    let deck_mock = cassette::start("momo/markji/cases/get_deck").await;

    let client = Client::new();
    let decks: Value = client
        .get(decks_mock.url(&format!("{API}/decks")))
        .query(&[("offset", "0"), ("limit", "10")])
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let deck: Value = client
        .get(deck_mock.url(&format!("{API}/decks/d_1")))
        .query(&[("with_root", "false")])
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(decks["data"]["total"], 2);
    assert_eq!(decks["data"]["decks"][1]["source"], "FORK");
    assert_eq!(deck["data"]["deck"]["name"], "N2 单词");
}

#[tokio::test]
async fn markji_list_folders_native_yaml() {
    let mock = cassette::start("momo/markji/cases/list_folders").await;

    let resp = Client::new()
        .get(mock.url(&format!("{API}/decks/folders")))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["data"]["folders"][0]["name"], "日语");
    assert_eq!(
        body["data"]["folders"][0]["items"][1]["object_class"],
        "FOLDER"
    );
}

// ============================================================
// cassette：单接口用例（<项目>/cases/，每个用例自己一个 server）
// ============================================================

/// 按请求体区分结果，同一个请求按次数返回不同 token
#[tokio::test]
async fn moji_cases_login() {
    let mock = cassette::start("moji/cases/login").await;
    let client = Client::new();
    let login = |body: Value| client.post(mock.url(LOGIN)).json(&body).send();

    let wrong = login(
        json!({ "authName": "PasswordAuth", "authPayload": { "email": "a@b.c", "code": "wrong" } }),
    )
    .await
    .unwrap();
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        wrong.json::<Value>().await.unwrap()["message"],
        "email or password error"
    );

    let sms = login(json!({ "authName": "SmsAuth" })).await.unwrap();
    assert_eq!(sms.json::<Value>().await.unwrap()["code"], 100000001);

    let ok =
        json!({ "authName": "PasswordAuth", "authPayload": { "email": "a@b.c", "code": "right" } });
    for expected in ["t1", "t2", "t2"] {
        let body: Value = login(ok.clone()).await.unwrap().json().await.unwrap();
        assert_eq!(body["sessionToken"], expected);
    }
}

/// 单接口的 server 也能有自己的 state：连续输错 3 次后锁定
#[tokio::test]
async fn moji_cases_login_lock() {
    let mock = cassette::start("moji/cases/login_lock").await;
    let client = Client::new();
    let login = |password: &'static str| {
        client
            .post(mock.url(LOGIN))
            .json(&json!({ "authName": "PasswordAuth", "authPayload": { "code": password } }))
            .send()
    };

    for _ in 0..3 {
        assert_eq!(
            login("bad").await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        login("nyan123").await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    #[derive(Deserialize)]
    struct Lock {
        failures: i64,
    }
    assert_eq!(mock.state::<Lock>().failures, 3);
}

/// 脚本取请求数据的钩子
#[tokio::test]
async fn script_request_hooks() {
    let mock = cassette::start("common/cases/echo").await;
    let client = Client::new();
    let url = mock.url("/echo?q=hello&tag=a&tag=b");

    let json: Value = client
        .post(&url)
        .header("x-token", "abc")
        .json(&json!({ "user": { "id": 7 } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(json["method"], "POST");
    assert_eq!(json["q"], "hello");
    assert_eq!(json["tags"], json!(["a", "b"]));
    assert_eq!(json["missing"], Value::Null);
    assert_eq!(json["token"], "abc"); // 名字不分大小写
    assert_eq!(json["body"]["user"]["id"], 7);

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

    // 文本按 charset 解码：GBK 编码的「猫」
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
    assert_eq!(text["calls"], 3);
}
