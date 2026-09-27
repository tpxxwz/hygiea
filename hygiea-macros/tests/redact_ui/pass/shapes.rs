// 各种能编译通过的写法：结构体 / tuple struct / enum、泛型（生命周期、类型、常量参数）、
// 和已有的 serialize_with / with / skip_serializing_if 组合、带路径的 derive、不 import 宏直接写全路径
use hygiea::redact::{self, redact};
use serde::{Deserialize, Serialize};

mod upper {
    #[allow(clippy::ptr_arg)]
    pub fn ser<S: serde::Serializer>(v: &String, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_uppercase())
    }
}

mod hex {
    pub fn serialize<S: serde::Serializer>(v: &u32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{v:x}"))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        let s: String = serde::Deserialize::deserialize(d)?;
        u32::from_str_radix(&s, 16).map_err(serde::de::Error::custom)
    }
}

#[redact]
#[derive(Serialize, Deserialize)]
struct Composed<'a, T: Serialize, const N: usize> {
    #[redact(mask)]
    #[serde(serialize_with = "upper::ser")]
    a: String,
    #[redact(mask)]
    #[serde(with = "hex")]
    b: u32,
    #[redact(skip)]
    #[serde(skip_serializing_if = "str::is_empty")]
    c: &'a str,
    #[serde(skip)]
    t: Option<T>,
}

#[redact]
#[derive(serde::Serialize)]
struct Tuple(u8, #[redact(mask)] String);

#[hygiea::redact::redact]
#[derive(Debug)]
#[derive(Serialize)]
enum Event {
    Named {
        #[redact(mask)]
        token: String,
    },
    Unit,
}

fn main() {
    let v = Composed::<u8, 2> { a: "a".into(), b: 1, c: "c", t: None };
    assert_eq!(redact::to_redacted_json(&v).unwrap(), r#"{"a":"***","b":"***"}"#);
    let _ = redact::to_redacted_json(&Tuple(1, "s".into()));
    let _ = redact::to_redacted_json(&[Event::Named { token: "t".into() }, Event::Unit]);
}
