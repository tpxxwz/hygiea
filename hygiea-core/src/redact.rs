//! 日志打码：让字段只在日志里打码或不出现，平时的序列化（发请求、存库、回响应）不受影响。
//!
//! 和具体用在哪无关：http 模块的请求/响应日志用它，自己打结构化日志时也可以用 [`to_redacted_json`]。
//! 只有经过 [`to_redacted_json`] 序列化时才会打码；`serde_json::to_string`、reqwest 的 `.json()`、
//! 存库、回响应这些普通序列化照常输出原文，`Debug`（`{:?}`、`tracing::info!(?x)`）也不打码。
//! 日志里要打码就用 [`to_redacted_json`] 的结果。
//!
//! 一般用 [`redact`] 属性宏，写在 `#[derive(Serialize)]` 上面，字段上标
//! `#[redact(mask)]`（显示成 `"***"`）或 `#[redact(skip)]`（不出现）：
//!
//! ```ignore
//! #[redact]
//! #[derive(Serialize, Deserialize)]
//! struct KeyInfo {
//!     name: String,
//!     #[redact(mask)]
//!     secret: String,
//!     #[redact(skip)]
//!     device_id: String,
//! }
//! ```
//!
//! 打码做在 `Serialize` 里：[`to_redacted_json`] 先打开一个线程局部的「日志模式」开关，再序列化，
//! 标了的字段在这个模式下输出 `"***"` 或者被跳过；开关只在那一次序列化期间打开，序列化是同步的，
//! 不会串到别处。所以 serde 序列化到哪一层，打码就跟到哪一层——嵌套结构体、`Vec`、`HashMap`、
//! `Option`、泛型外层（比如 `ApiResult<Vec<KeyInfo>>`）里的都一样，不需要额外标记。
//!
//! 属性宏只是语法糖，展开后就是 `#[serde(serialize_with = ..)]` / `#[serde(skip_serializing_if = ..)]`
//! 指向本模块的 [`mask`] / [`skip`]。手写 `impl Serialize` 的类型可以直接用 [`active`] 和
//! [`write_masked`]

use std::cell::Cell;

use serde::{Serialize, Serializer};

use crate::{BaseErr, HyErr, ResultExt, err};

pub use hygiea_macros::redact;

/// 属性宏生成的辅助函数引用这个，下游不必自己依赖 serde 的路径
#[doc(hidden)]
pub use serde::Serializer as __Serializer;

/// 打码后的占位
pub const MASKED: &str = "***";

thread_local! {
    /// 嵌套深度而不是 bool：日志预览里再调 to_redacted_json 也不会提前关掉
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// 当前是否在生成日志预览
pub fn active() -> bool {
    DEPTH.get() > 0
}

/// `serialize_with` 用：日志模式下输出 [`MASKED`]，否则照常序列化
pub fn mask<T: Serialize + ?Sized, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if active() {
        write_masked(serializer)
    } else {
        value.serialize(serializer)
    }
}

/// `skip_serializing_if` 用：日志模式下跳过这个字段
pub fn skip<T: ?Sized>(_: &T) -> bool {
    active()
}

/// 写一个 [`MASKED`]。字段本身不实现 `Serialize`（有自己的 `serialize_with`）时，组合用的就是它
pub fn write_masked<S: Serializer>(serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(MASKED)
}

/// 打开日志模式序列化成 JSON，标了的字段打码，也就是日志里该打的那一份。
/// 保留原始结构和数值类型。序列化失败时返回 `JsonError`
pub fn to_redacted_json<T: Serialize + ?Sized>(value: &T) -> Result<String, HyErr> {
    scope(|| serde_json::to_string(value))
        .wrap_err(|| err!(BaseErr::JsonError, "serialize log preview failed"))
}

/// 在日志模式下执行 `f`：期间当前线程上的任何 serde 序列化（JSON、urlencoded……）都会打码。
/// http 模块用它按 reqwest 自己的编码方式拼出打码后的 URL
pub(crate) fn scope<R>(f: impl FnOnce() -> R) -> R {
    // f 中途 panic 也要把深度减回去，不然这个线程之后的正常序列化全被打码。
    // 必须绑定到具名变量：写成 `let _ = Guard;` 会当场 drop，开关立刻就关了
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            DEPTH.set(DEPTH.get() - 1);
        }
    }
    DEPTH.set(DEPTH.get() + 1);
    let _guard = Guard;
    f()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use hygiea_test_support::Unserializable;
    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::*;

    /// (日志模式下的输出, 平时的输出)
    fn both<T: Serialize>(v: &T) -> (Value, Value) {
        let log = serde_json::from_str(&to_redacted_json(v).unwrap()).unwrap();
        (log, serde_json::to_value(v).unwrap())
    }

    /// 日志模式下只看输出
    fn log_of<T: Serialize>(v: &T) -> Value {
        both(v).0
    }

    #[redact]
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    struct Key {
        name: String,
        #[redact(mask)]
        secret: String,
        #[redact(skip)]
        device_id: String,
    }

    fn key() -> Key {
        Key {
            name: "k".into(),
            secret: "S".into(),
            device_id: "D".into(),
        }
    }

    /// `key()` 在日志里的样子：secret 打码、device_id 不出现
    fn key_log() -> Value {
        json!({"name": "k", "secret": "***"})
    }

    /// `key()` 平时序列化的样子
    fn key_raw() -> Value {
        json!({"name": "k", "secret": "S", "device_id": "D"})
    }

    /// 日志模式开关本身
    mod mode {
        use super::*;

        /// 平时是关的
        #[test]
        fn inactive_by_default() {
            assert!(!active());
        }

        /// 只在 to_redacted_json 那一次序列化期间打开，结束就关
        #[test]
        fn active_only_during_to_json() {
            struct Probe;
            impl Serialize for Probe {
                fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                    s.serialize_bool(active())
                }
            }
            assert_eq!(to_redacted_json(&Probe).unwrap(), "true");
            assert_eq!(serde_json::to_string(&Probe).unwrap(), "false");
            assert!(!active());
        }

        /// 日志预览里再调一次 to_redacted_json，出来之后外层仍在日志模式（按深度计数，不是 bool）
        #[test]
        fn nested_to_json_keeps_outer_mode() {
            struct Inner;
            impl Serialize for Inner {
                fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                    let _ = to_redacted_json(&1).unwrap();
                    s.serialize_bool(active())
                }
            }
            assert_eq!(to_redacted_json(&Inner).unwrap(), "true");
            assert!(!active());
        }

        /// 序列化失败：错误原样返回（JsonError），开关照样复位
        #[test]
        fn reset_after_error() {
            assert!(
                to_redacted_json(&Unserializable)
                    .unwrap_err()
                    .is(BaseErr::JsonError)
            );
            assert!(!active());
        }

        /// 序列化中途 panic：开关照样复位，这个线程之后的正常序列化不受影响
        #[test]
        fn reset_after_panic() {
            struct Boom;
            impl Serialize for Boom {
                fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                    panic!("boom")
                }
            }
            assert!(std::panic::catch_unwind(|| to_redacted_json(&Boom)).is_err());
            assert!(!active());
            assert_eq!(both(&key()).1, key_raw());
        }

        /// 开关是线程局部的：一个线程在日志模式里，别的线程照常序列化
        #[test]
        fn other_threads_are_unaffected() {
            struct SpawnsThread;
            impl Serialize for SpawnsThread {
                fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                    let other = std::thread::spawn(|| (active(), serde_json::to_string(&key())))
                        .join()
                        .unwrap();
                    assert!(!other.0);
                    assert_eq!(
                        serde_json::from_str::<Value>(&other.1.unwrap()).unwrap(),
                        key_raw()
                    );
                    s.serialize_bool(active())
                }
            }
            assert_eq!(to_redacted_json(&SpawnsThread).unwrap(), "true");
        }
    }

    /// 不用属性宏，直接把辅助函数写进 serde 属性
    mod helpers {
        use super::*;

        #[derive(Serialize)]
        struct Manual {
            #[serde(serialize_with = "mask")]
            secret: String,
            #[serde(skip_serializing_if = "skip")]
            hidden: u8,
        }

        /// `mask` / `skip` 手写进 serde 属性也能用，效果和属性宏一样
        #[test]
        fn mask_and_skip_in_serde_attrs() {
            let v = Manual {
                secret: "S".into(),
                hidden: 1,
            };
            assert_eq!(
                both(&v),
                (json!({"secret":"***"}), json!({"secret":"S","hidden":1}))
            );
        }

        /// `write_masked` 直接写出占位符 `MASKED`
        #[test]
        fn write_masked_writes_placeholder() {
            let mut out = Vec::new();
            write_masked(&mut serde_json::Serializer::new(&mut out)).unwrap();
            assert_eq!(out, format!("\"{MASKED}\"").as_bytes());
        }

        /// 手写 Serialize 的类型用 `active()` 自己决定日志里打什么
        #[test]
        fn hand_written_serialize_can_use_active() {
            struct Card(&'static str);
            impl Serialize for Card {
                fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                    if active() {
                        s.serialize_str(&format!("****{}", &self.0[self.0.len() - 4..]))
                    } else {
                        s.serialize_str(self.0)
                    }
                }
            }
            let card = Card("6222000011112222");
            assert_eq!(both(&card), (json!("****2222"), json!("6222000011112222")));
        }
    }

    /// 属性宏的基本效果
    mod attribute_basics {
        use super::*;

        /// mask 显示 `***`，skip 不出现；平时序列化照常输出原文
        #[test]
        fn mask_and_skip_only_in_log_mode() {
            assert_eq!(both(&key()), (key_log(), key_raw()));
        }

        /// 反序列化完全不受影响，skip 的字段照样读进来
        #[test]
        fn deserialize_is_unaffected() {
            let k: Key = serde_json::from_value(key_raw()).unwrap();
            assert_eq!(k, key());
        }

        #[redact]
        #[derive(Serialize)]
        struct Optional {
            #[redact(mask)]
            masked: Option<String>,
            #[redact(skip)]
            skipped: Option<String>,
        }

        /// Option 字段 mask 时连 None 也显示成 `***`，看不出有没有值
        #[test]
        fn masked_option_hides_presence() {
            let v = Optional {
                masked: None,
                skipped: None,
            };
            assert_eq!(
                both(&v),
                (
                    json!({"masked":"***"}),
                    json!({"masked":null,"skipped":null})
                )
            );
        }

        #[redact]
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Renamed {
            #[redact(mask)]
            api_key: String,
            #[redact(mask)]
            #[serde(rename = "sig")]
            signature: String,
            plain_value: u8,
        }

        /// rename / rename_all 之后照样打码，不依赖 key 名
        #[test]
        fn works_with_renamed_fields() {
            let v = Renamed {
                api_key: "k".into(),
                signature: "s".into(),
                plain_value: 1,
            };
            assert_eq!(
                log_of(&v),
                json!({"apiKey":"***","sig":"***","plainValue":1})
            );
        }
    }

    /// 打码跟着 serde 进到每一层，不需要额外标记
    mod layers {
        use super::*;

        #[redact]
        #[derive(Serialize)]
        struct Envelope<'a, T> {
            code: &'a str,
            #[redact(skip)]
            trace_id: &'a str,
            data: T,
        }

        #[derive(Serialize)]
        struct Outer {
            inner: Key,
            list: Vec<Key>,
        }

        /// 顶层就是数组时，每个元素都打码
        #[test]
        fn top_level_array() {
            assert_eq!(log_of(&vec![key(), key()]), json!([key_log(), key_log()]));
        }

        /// 嵌套的结构体字段和字段里的数组；外层类型自己不需要属性宏
        #[test]
        fn nested_struct_fields() {
            let v = Outer {
                inner: key(),
                list: vec![key()],
            };
            assert_eq!(log_of(&v), json!({"inner": key_log(), "list": [key_log()]}));
        }

        /// map 的值、Option、Box、元组里的都打码
        #[test]
        fn maps_options_boxes_tuples() {
            let map = HashMap::from([("x", key())]);
            assert_eq!(log_of(&map), json!({"x": key_log()}));
            assert_eq!(log_of(&Some(key())), key_log());
            assert_eq!(log_of(&(Box::new(key()), 1)), json!([key_log(), 1]));
        }

        /// 泛型外层：外层自己的标记和 T 里的标记同时生效
        #[test]
        fn generic_envelope() {
            let v = Envelope {
                code: "0",
                trace_id: "t",
                data: vec![key()],
            };
            assert_eq!(log_of(&v), json!({"code": "0", "data": [key_log()]}));
        }

        /// T 是没有打码规则的类型时照常工作，原样输出
        #[test]
        fn generic_envelope_with_plain_data() {
            let v = Envelope {
                code: "0",
                trace_id: "t",
                data: vec![1.5f64],
            };
            assert_eq!(log_of(&v), json!({"code":"0","data":[1.5]}));
        }
    }

    /// 和字段上已有的 serde 属性组合
    mod compose {
        use super::*;

        mod upper {
            // serialize_with 传进来的是字段的引用，签名只能是 &String
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

        fn is_empty(s: &str) -> bool {
            s.is_empty()
        }

        /// 泛型（生命周期、类型、常量参数都有）上组合三种已有属性
        #[redact]
        #[derive(Serialize, Deserialize, Debug, PartialEq)]
        struct Composed<'a, T: Serialize, const N: usize> {
            #[redact(mask)]
            #[serde(serialize_with = "upper::ser")]
            a: String,
            #[redact(mask)]
            #[serde(with = "hex", rename = "bee")]
            b: u32,
            #[redact(skip)]
            #[serde(skip_serializing_if = "is_empty")]
            c: &'a str,
            #[serde(skip)]
            t: Option<T>,
        }

        fn composed(c: &str) -> Composed<'_, u8, 2> {
            Composed {
                a: "abc".into(),
                b: 255,
                c,
                t: None,
            }
        }

        /// 日志模式下打码；平时走用户原来的 serialize_with / with
        #[test]
        fn existing_serialize_with_and_with() {
            assert_eq!(
                both(&composed("cc")),
                (
                    json!({"a":"***","bee":"***"}),
                    json!({"a":"ABC","bee":"ff","c":"cc"})
                )
            );
        }

        /// 用户自己的 skip_serializing_if 平时照样生效
        #[test]
        fn existing_skip_serializing_if() {
            assert_eq!(both(&composed("")).1, json!({"a":"ABC","bee":"ff"}));
        }

        /// `with` 的反序列化那半保留
        #[test]
        fn with_keeps_deserialize() {
            let back: Composed<u8, 2> =
                serde_json::from_str(r#"{"a":"x","bee":"ff","c":""}"#).unwrap();
            assert_eq!(back.b, 255);
        }
    }

    /// 各种类型形态
    mod shapes {
        use super::*;

        #[redact]
        #[derive(Serialize)]
        struct Tuple(u8, #[redact(mask)] String, #[redact(skip)] String);

        #[redact]
        #[derive(Serialize)]
        enum Event {
            Named {
                #[redact(mask)]
                token: String,
                n: u8,
            },
            Tuple(#[redact(mask)] String, u8),
            Skipped(#[redact(skip)] String, u8),
        }

        #[redact]
        #[derive(Serialize)]
        struct Flattened {
            a: u8,
            #[redact(skip)]
            #[serde(flatten)]
            extra: HashMap<String, String>,
            #[serde(flatten)]
            inner: Key,
        }

        #[redact]
        #[derive(Serialize)]
        #[serde(transparent)]
        struct Token(#[redact(mask)] String);

        /// tuple struct 的字段
        #[test]
        fn tuple_struct() {
            assert_eq!(
                both(&Tuple(1, "S".into(), "D".into())),
                (json!([1, "***"]), json!([1, "S", "D"]))
            );
        }

        /// enum 的具名字段、元组字段都能标
        #[test]
        fn enum_variant_fields() {
            let events = vec![
                Event::Named {
                    token: "S".into(),
                    n: 1,
                },
                Event::Tuple("S".into(), 2),
                Event::Skipped("D".into(), 3),
            ];
            assert_eq!(
                log_of(&events),
                json!([{"Named":{"token":"***","n":1}},{"Tuple":["***",2]},{"Skipped":[3]}])
            );
        }

        /// flatten 字段可以 skip；被 flatten 的内层类型自己的标记照样生效
        #[test]
        fn flatten() {
            let v = Flattened {
                a: 1,
                extra: HashMap::from([("k".to_string(), "v".to_string())]),
                inner: key(),
            };
            assert_eq!(log_of(&v), json!({"a":1,"name":"k","secret":"***"}));
        }

        /// transparent 的唯一字段 mask，整个值显示成 `***`
        #[test]
        fn transparent() {
            assert_eq!(both(&Token("S".into())), (json!("***"), json!("S")));
        }
    }
}
