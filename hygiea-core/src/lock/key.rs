//! 锁的 key：
//! - 整数 key [`LockKeyI32`] / [`LockKeyI64`]：能 `Into<i32>` / `Into<i64>` 的类型都能传（和标准库一致），另外多了 `u32` / `u64`，按位转
//! - 锁类型 [`LockType`]（`try_lock_i32` 的 `key1`）、字符串 key [`LockKey`]：只能由注册过的 derive 生成
//!
//! 后半部分是编号的注册：`#[derive(hy_lock_type)]`、`#[derive(hy_lock_key)]` 和 hygiea 内置保留的编号共用一套 9 位编号，
//! 启动时查重、建字符串 key 的模板

use std::collections::HashMap;
use std::num::{NonZeroI32, NonZeroI64};
use std::sync::OnceLock;

use linkme::distributed_slice;
use minijinja::Environment;

use crate::builtin::LOCK_CODE;

/// `i32` 的 key，[`try_lock_i32`](super::DistributedLock::try_lock_i32) 的参数。
///
/// 标准库能 `Into<i32>` 的类型照原值转；`u32` 按位转（`u32::MAX` 是 `-1`），一一对应、不会撞
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LockKeyI32(i32);

/// `i64` 的 key，[`try_lock_i64`](super::DistributedLock::try_lock_i64) 的参数。
///
/// 标准库能 `Into<i64>` 的类型照原值转；`u64` 按位转（`u64::MAX` 是 `-1`），一一对应、不会撞
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LockKeyI64(i64);

impl LockKeyI32 {
    /// 后端用的值
    pub fn get(self) -> i32 {
        self.0
    }
}

impl LockKeyI64 {
    /// 后端用的值
    pub fn get(self) -> i64 {
        self.0
    }

    /// 两个 `i32` 合成一个：高 32 位 `key1`，低 32 位 `key2`，一一对应。
    /// 低 32 位按无符号拼，负数不会把高位冲掉
    pub fn from_pair(key1: LockKeyI32, key2: LockKeyI32) -> Self {
        Self(((key1.0 as i64) << 32) | (key2.0 as u32 as i64))
    }

    /// 字符串 key 哈希成 `i64` key，交给 `*_i64` 的锁：不需要后端支持字符串 key（比如 PG 不建锁表）。
    ///
    /// 有损的降级，由调用方显式选：不同 key 可能撞到同一个值（多锁，不会少锁）；和直接传数字的 `try_lock_i64`
    /// 共用一个空间；和同一个字符串 key 走 `try_lock` 的那把锁是两样东西，互不互斥。
    /// 算法固定是 [`string::xxh3_64`](crate::string::xxh3_64)，跨进程、跨机器、跨版本结果一致。
    /// 要哈希就都用它，不要自己换别的哈希（尤其是 `DefaultHasher`，每个进程种子不同，锁会失效）。
    ///
    /// ```ignore
    /// db.try_lock_i64(LockKeyI64::xxh3_64(&lock_key!(OrderLockKey::Export, user_name))?).await?;
    /// ```
    pub fn xxh3_64(key: &LockKey) -> crate::Result<Self> {
        Ok(Self(crate::string::xxh3_64(key.as_str()?)))
    }
}

/// 锁的类型，`try_lock_i32` 的 `key1`，只能由 `#[derive(hy_lock_type)]` 生成：值是项目前缀 3 位 + 模块前缀 + `code`，
/// 共 9 位，规则和错误码一样（项目前缀也和错误码共用）。
///
/// ```ignore
/// #[derive(hy_lock_type)]
/// #[lock_module_prefix = "02"]
/// pub enum OrderLockType {
///     #[lock_type(code = "0001")]
///     Pay,           // 项目前缀 101 时值是 101020001
/// }
///
/// lock.try_lock_i32(OrderLockType::Pay, order_id).await?;
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LockType(pub(super) LockKeyI32);

impl LockType {
    /// 后端用的值，和 `key2` 一样是 [`LockKeyI32`]
    pub fn key(self) -> LockKeyI32 {
        self.0
    }
}

/// 字符串 key，`try_lock` 的参数，只能由 `lock_key!` 生成：`<9 位编号>:<模板渲染结果>`。
/// 编号和锁类型共用一套、全局唯一，不同模板渲染出来的 key 不会撞。
///
/// ```ignore
/// #[derive(hy_lock_key)]
/// #[lock_module_prefix = "02"]
/// pub enum OrderLockKey {
///     #[lock_key(code = "0003", tpl = "daily_settle")]
///     DailySettle,
///     #[lock_key(code = "0004", tpl = "export:{{ user }}")]
///     Export,
/// }
///
/// lock.try_lock(lock_key!(OrderLockKey::DailySettle)).await?;       // "101020003:daily_settle"
/// lock.try_lock(lock_key!(OrderLockKey::Export, user_name)).await?;  // "101020004:export:alice"
/// ```
///
/// 模板有多个变量时，变量的值里不要带模板里的分隔符：模板 `{{ a }}:{{ b }}` 下 `a = "x:y", b = "z"` 和
/// `a = "x", b = "y:z"` 渲染出来一样，是同一把锁。传 id、日期、数字不会有这个问题。
///
/// **key 要尽量短**：后端可以限制 key 的长度，超长的 key 加锁时返回 [`LockErr::InvalidKey`](super::LockErr::InvalidKey)，
/// 不截断、不哈希，也不发给后端。开头的 `<9 位编号>:` 固定占 10 个字符，留给模板渲染结果的只有余下的部分；
/// 上限各后端不同，见所用后端的文档。变量的值长度不固定（名字、路径这类）时，按最长的情况算
///
/// 模板渲染失败（`{ .. }` 写法里 key 用了表达式，编译期查不了，运行时缺变量）时 [`as_str`](Self::as_str) 返回错误
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LockKey(pub(super) std::result::Result<String, String>);

impl LockKey {
    /// 后端用的 key；模板渲染失败时是 [`LockErr::InvalidKey`](super::LockErr::InvalidKey)
    pub fn as_str(&self) -> crate::Result<&str> {
        match &self.0 {
            Ok(key) => Ok(key),
            Err(cause) => Err(crate::err!(super::LockErr::InvalidKey, cause)),
        }
    }
}

macro_rules! impl_from {
    ($key:ident, $inner:ty; lossless: $($from:ty),*; bitwise: $bitwise:ty) => {
        $(
            impl From<$from> for $key {
                fn from(value: $from) -> Self {
                    Self(<$inner>::from(value))
                }
            }
        )*
        impl From<$bitwise> for $key {
            fn from(value: $bitwise) -> Self {
                Self(value as $inner)
            }
        }
    };
}

impl_from!(LockKeyI32, i32; lossless: i8, i16, i32, u8, u16, bool, NonZeroI32; bitwise: u32);
impl_from!(LockKeyI64, i64; lossless: i8, i16, i32, i64, u8, u16, u32, bool, NonZeroI64; bitwise: u64);

impl From<LockKeyI32> for LockKeyI64 {
    fn from(value: LockKeyI32) -> Self {
        Self(value.0.into())
    }
}

// ---- 注册：编号、查重、字符串 key 的模板 ----

#[doc(hidden)]
pub struct LockRegistration {
    /// 9 位编号，查重按它；也是字符串 key 的前缀、模板的名字
    pub code: &'static str,
    /// `Enum::Variant`，重复时报出来
    pub name: &'static str,
    /// `hy_lock_key` 的模板，锁类型是 `None`
    pub tpl: Option<&'static str>,
}

#[doc(hidden)]
#[distributed_slice]
pub static LOCK_REGISTRATIONS: [LockRegistration] = [..];

/// 内置保留的锁编号（[`crate::builtin::LOCK_CODE`]）占一条，业务注册到同一个编号时启动查重报错
#[distributed_slice(LOCK_REGISTRATIONS)]
static BUILTIN_REGISTRATION: LockRegistration = LockRegistration {
    code: LOCK_CODE,
    name: "hygiea::builtin::LOCK_CODE",
    tpl: None,
};

static LOCK_TEMPLATES: OnceLock<Environment<'static>> = OnceLock::new();

/// 宏生成的 `From<Enum> for LockType` 用，值已经注册过
#[doc(hidden)]
pub fn registered(value: i32) -> LockType {
    LockType(value.into())
}

/// `lock_key!` 展开后调用：按编号取模板渲染，拼成 `<编号>:<渲染结果>`
#[doc(hidden)]
pub fn lock_key(code: &'static str, args: Option<serde_json::Value>) -> LockKey {
    let rendered = templates()
        .get_template(code)
        .and_then(|tpl| tpl.render(args.unwrap_or_default()))
        .map(|rendered| format!("{code}:{rendered}"))
        .map_err(|e| format!("{code}: {e}"));
    LockKey(rendered)
}

/// i64 key 转成的字符串 key：`999999999:<数字>`
pub(super) fn i64_key(key: i64) -> LockKey {
    LockKey(Ok(format!("{LOCK_CODE}:{key}")))
}

/// 进程启动时（core 的 ctor）调用：发现跨 crate 重复的编号、建模板。和错误码一样，ctor 里不 panic，
/// 打一行报错后 `exit(1)`
pub(crate) fn init() {
    match build(LOCK_REGISTRATIONS) {
        Ok(env) => {
            let _ = LOCK_TEMPLATES.set(env);
        }
        Err(msg) => {
            eprintln!("hygiea: {msg}");
            std::process::exit(1);
        }
    }
}

/// 一般在启动时由 [`init`] 建好；下游在自己的 ctor 里比 core 先用到时现场建。失败（重复编号）不 panic，
/// 返回空的 env，渲染时报错，随后 core 的 ctor 会发现同一个问题并退出进程
fn templates() -> &'static Environment<'static> {
    LOCK_TEMPLATES.get_or_init(|| build(LOCK_REGISTRATIONS).unwrap_or_else(|_| Environment::new()))
}

/// 查重（锁类型、字符串 key、内置编号一起），再用字符串 key 的模板建环境（和错误码共用 `build_env`）
fn build<'a>(
    regs: impl IntoIterator<Item = &'a LockRegistration>,
) -> Result<Environment<'static>, String> {
    let regs: Vec<_> = regs.into_iter().collect();
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for reg in &regs {
        if let Some(first) = seen.insert(reg.code, reg.name) {
            return Err(format!(
                "duplicate lock code {}: {first} and {}",
                reg.code, reg.name
            ));
        }
    }
    crate::error::build_env(
        regs.iter()
            .filter_map(|reg| reg.tpl.map(|tpl| (reg.code, tpl))),
        "lock code",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{hy_lock_key, hy_lock_type};

    #[test]
    fn unsigned_converts_bitwise() {
        assert_eq!(LockKeyI32::from(u32::MAX).get(), -1);
        assert_eq!(LockKeyI32::from(7u32).get(), 7);
        assert_eq!(LockKeyI64::from(u64::MAX).get(), -1);
        assert_eq!(LockKeyI64::from(u32::MAX).get(), u32::MAX as i64);
        assert_eq!(LockKeyI64::from(-3i16).get(), -3);
        assert_eq!(LockKeyI32::from(true).get(), 1);
        assert_eq!(LockKeyI64::from(NonZeroI64::MIN).get(), i64::MIN);
    }

    /// 就是 string::xxh3_64（算法固定的测试在那边）；渲染失败的 key 报错
    #[test]
    fn xxh3_64_from_string_key() {
        let key = |s: &str| LockKey(Ok(s.to_string()));
        let hashed = LockKeyI64::xxh3_64(&key("001020002:settle:2026-10-13"))
            .unwrap()
            .get();
        assert_eq!(
            hashed,
            crate::string::xxh3_64("001020002:settle:2026-10-13")
        );
        let invalid = LockKeyI64::xxh3_64(&LockKey(Err("missing id".to_string()))).unwrap_err();
        assert!(invalid.is(crate::lock::LockErr::InvalidKey));
    }

    #[test]
    fn pair_is_one_to_one() {
        let pair = |a: i32, b: i32| LockKeyI64::from_pair(a.into(), b.into()).get();
        assert_eq!(pair(0, 42), 42);
        assert_eq!(pair(1, -1), (1i64 << 32) | 0xFFFF_FFFF);
        assert_eq!(pair(-1, 0), -1i64 << 32);
        assert_ne!(pair(0, -1), pair(-1, -1));
    }

    #[derive(hy_lock_type)]
    #[lock_internal_module_prefix = "090"]
    enum TestLockType {
        #[lock_type(code = "001")]
        A,
        #[lock_type(code = "002")]
        B,
    }

    /// 和上面的锁类型共用编号，code 不能和它们重复
    #[derive(hy_lock_key)]
    #[lock_internal_module_prefix = "090"]
    enum TestLockKey {
        #[lock_key(code = "003", tpl = "fixed")]
        Fixed,
        #[lock_key(code = "004", tpl = "user:{{ id }}")]
        User,
        #[lock_key(code = "005", tpl = "{{ a }}:{{ b }}")]
        Pair,
    }

    #[test]
    fn lock_type_value_is_prefixes_plus_code() {
        assert_eq!(LockType::from(TestLockType::A).key().get(), 999_090_001);
        assert_eq!(LockType::from(TestLockType::B).key().get(), 999_090_002);
    }

    #[test]
    fn lock_key_renders_with_code_prefix() {
        let key = |k: LockKey| k.as_str().unwrap().to_string();
        assert_eq!(key(crate::lock_key!(TestLockKey::Fixed)), "999090003:fixed");
        assert_eq!(
            key(crate::lock_key!(TestLockKey::User, 42)),
            "999090004:user:42"
        );
        assert_eq!(
            key(crate::lock_key!(TestLockKey::Pair, { "a": "x", "b": 7 })),
            "999090005:x:7"
        );
        assert_eq!(i64_key(-5).as_str().unwrap(), "999999999:-5");
    }

    /// `{ .. }` 里 key 用了表达式，编译期查不了，缺的变量在 as_str 时报错
    #[test]
    fn missing_variable_is_an_error() {
        let name = "a";
        let key = crate::lock_key!(TestLockKey::Pair, { (name): "x" });
        assert!(
            key.as_str()
                .unwrap_err()
                .is(crate::lock::LockErr::InvalidKey)
        );
    }

    /// 进程里实际注册的编号没有重复，测试用的、内置的都注册进去了
    #[test]
    fn registered_codes_are_unique() {
        assert!(build(LOCK_REGISTRATIONS).is_ok());
        let has = |code: &str, name: &str| {
            LOCK_REGISTRATIONS
                .iter()
                .any(|reg| reg.code == code && reg.name == name)
        };
        assert!(has("999090001", "TestLockType::A"));
        assert!(has("999090004", "TestLockKey::User"));
        assert!(has(LOCK_CODE, "hygiea::builtin::LOCK_CODE"));
    }

    #[test]
    fn duplicates_rejected() {
        let reg = |code: &'static str, name: &'static str| LockRegistration {
            code,
            name,
            tpl: None,
        };
        let regs = [
            reg("000000001", "X::A"),
            reg("000000002", "X::B"),
            reg("000000001", "Y::C"),
        ];
        let msg = build(&regs).err().unwrap();
        assert!(msg.contains("X::A") && msg.contains("Y::C"), "{msg}");
    }
}
