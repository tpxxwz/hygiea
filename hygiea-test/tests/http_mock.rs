//! http_mock 的集成测试，用 `tests/resources/httpmock/common/` 下的示例服务：
//! server/ 是一个小商店（登录、商品列表、商品详情、下载文件），cases/ 是单接口用例。
//! 客户端用原生 reqwest。跑法：
//! cargo test -p hygiea-test --features http-mock --test http_mock

use std::time::{Duration, Instant};

use hygiea_test::cassette_root;
use hygiea_test::http_mock::{Cassette, HttpMockErr, Mocked};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

const SHOP_DB: &str = "common/server/data/db.json";

const SHOP: [&str; 4] = [
    "common/server/login",
    "common/server/items",
    "common/server/item",
    "common/server/file",
];

async fn login(client: &Client, mock: &Mocked, name: &str, password: &str) -> reqwest::Response {
    client
        .post(mock.url("/api/login"))
        .json(&json!({ "name": name, "password": password }))
        .send()
        .await
        .unwrap()
}

// ============================================================
// server 模式：完整模拟服务
// ============================================================

#[derive(Deserialize)]
struct Db {
    token_seq: i64,
    sessions: serde_json::Map<String, Value>,
}

#[tokio::test]
async fn server_full_flow() {
    let mock = Cassette::load(cassette_root!(), &SHOP)
        .db(SHOP_DB)
        .start()
        .await
        .unwrap();
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

    // 详情：路径占位符；没有的 404
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

    // 下载：文本响应体里的 {{base}} 换成了 mock 地址；contract_raw 不看 body
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

#[tokio::test]
async fn case_login_by_body_and_sequence() {
    let mock = Cassette::load(cassette_root!(), &["common/cases/login"])
        .start()
        .await
        .unwrap();
    let client = Client::new();

    let wrong = login(&client, &mock, "alice", "wrong").await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    for expected in ["t1", "t2", "t2"] {
        let body: Value = login(&client, &mock, "alice", "x")
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(body["token"], expected);
    }
    mock.assert_valid();
}

#[tokio::test]
async fn case_with_own_db() {
    // 自己的一份数据，脚本复用 server 的（server 脚本里 import 的 common 也能找到）
    let mock = Cassette::load(cassette_root!(), &["common/cases/items_custom"])
        .db("common/cases/data/items_custom.json")
        .start()
        .await
        .unwrap();
    let body: Value = Client::new()
        .get(mock.url("/api/items"))
        .query(&[("q", "yo")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["id"], "x2");
    mock.assert_valid();
}

#[tokio::test]
async fn case_db_based_on_server() {
    // 数据是 server 的 db 加一个已登录的 tk-pre
    let mock = Cassette::load(cassette_root!(), &["common/cases/items_from_server"])
        .db("common/cases/data/items_from_server.json")
        .start()
        .await
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
async fn case_flaky_responses() {
    let mock = Cassette::load(cassette_root!(), &["common/cases/flaky"])
        .start()
        .await
        .unwrap();
    let client = Client::new();

    // delay_ms
    let started = Instant::now();
    client
        .get(mock.url("/api/items?slow=1"))
        .send()
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(300));

    // match_script：有 x-beta 头走灰度
    let beta: Value = client
        .get(mock.url("/api/items"))
        .header("x-beta", "1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(beta["next_cursor"], "c1");

    // 其他请求落到网关错误：返回 HTML
    let html = client.get(mock.url("/api/items")).send().await.unwrap();
    assert!(html.text().await.unwrap().contains("Bad Gateway"));
    mock.assert_valid();
}

#[tokio::test]
async fn case_request_hooks() {
    let mock = Cassette::load(cassette_root!(), &["common/cases/echo"])
        .start()
        .await
        .unwrap();
    let client = Client::new();
    let url = mock.url("/echo/42?q=hello&tag=a&tag=b");

    let json: Value = client
        .post(&url)
        .header("x-token", "abc")
        .header("cookie", "a=1; sid=s-9")
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
    assert_eq!(json["missing"], Value::Null);
    assert_eq!(json["token"], "abc");
    assert_eq!(json["sid"], "s-9");
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
    assert_eq!(text["calls"], 3);
    mock.assert_valid();
}

// ============================================================
// 加载错误
// ============================================================

#[tokio::test]
async fn script_error_is_reported() {
    let mock = Cassette::load(cassette_root!(), &["common/cases/broken"])
        .start()
        .await
        .unwrap();
    let resp = Client::new().get(mock.url("/broken")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let problems = mock.problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("[脚本出错] call #1"), "{problems:?}");
}

#[tokio::test]
async fn load_errors() {
    let start = |cassette: Cassette| async move { cassette.start().await.err().unwrap() };

    let missing = start(Cassette::load(cassette_root!(), &["common/cases/nope"])).await;
    assert!(missing.is(HttpMockErr::CassetteNotFound), "{missing}");

    let no_db_file =
        start(Cassette::load(cassette_root!(), &["common/cases/login"]).db("common/nope.json"))
            .await;
    assert!(no_db_file.is(HttpMockErr::ReadFailed), "{no_db_file}");

    let not_object = start(
        Cassette::load(cassette_root!(), &["common/cases/login"])
            .db("common/cases/data/not_object.json"),
    )
    .await;
    assert!(not_object.is(HttpMockErr::InvalidDb), "{not_object}");
}
