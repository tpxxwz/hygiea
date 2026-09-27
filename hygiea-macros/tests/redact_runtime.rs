//! 经 facade 的 `#[redact]` 派生，断言 `to_redacted_json` 的实际输出（不只是能编译）。

use hygiea::redact::{redact, to_redacted_json};
use serde::Serialize;

#[redact]
#[derive(Serialize)]
struct Inner {
    #[redact(mask)]
    token: String,
    visible: u32,
}

#[redact]
#[derive(Serialize)]
struct Outer {
    #[redact(mask)]
    password: String,
    #[redact(skip)]
    secret: String,
    inner: Inner,
    list: Vec<Inner>,
    opt: Option<Inner>,
}

fn sample(opt: Option<Inner>) -> Outer {
    Outer {
        password: "p@ss".into(),
        secret: "top-secret".into(),
        inner: Inner {
            token: "inner-token".into(),
            visible: 1,
        },
        list: vec![
            Inner {
                token: "t1".into(),
                visible: 2,
            },
            Inner {
                token: "t2".into(),
                visible: 3,
            },
        ],
        opt,
    }
}

#[test]
fn mask_replaces_value_and_skip_omits_field() {
    let json = to_redacted_json(&sample(None)).unwrap();
    assert_eq!(
        json,
        r#"{"password":"***","inner":{"token":"***","visible":1},"list":[{"token":"***","visible":2},{"token":"***","visible":3}],"opt":null}"#
    );
}

#[test]
fn nested_struct_field_is_masked() {
    let json = to_redacted_json(&sample(None)).unwrap();
    assert!(json.contains(r#""inner":{"token":"***","visible":1}"#));
}

#[test]
fn vec_elements_are_each_masked() {
    let json = to_redacted_json(&sample(None)).unwrap();
    assert!(json.contains(r#""list":[{"token":"***","visible":2},{"token":"***","visible":3}]"#));
}

#[test]
fn option_field_masked_when_some_and_null_when_none() {
    let with_some = to_redacted_json(&sample(Some(Inner {
        token: "opt-token".into(),
        visible: 9,
    })))
    .unwrap();
    assert!(with_some.ends_with(r#""opt":{"token":"***","visible":9}}"#));

    let with_none = to_redacted_json(&sample(None)).unwrap();
    assert!(with_none.ends_with(r#""opt":null}"#));
}

#[test]
fn plain_serde_json_is_not_redacted() {
    // 不经过 to_redacted_json，走普通的 serde_json::to_string 时不打码
    let json = serde_json::to_string(&sample(None)).unwrap();
    assert!(json.contains(r#""password":"p@ss""#));
    assert!(json.contains(r#""secret":"top-secret""#));
}
