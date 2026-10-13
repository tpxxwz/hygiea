//! 锁的注册表：`#[derive(hy_lock_type)]`、`#[derive(hy_lock_key)]` 和内置的 i64 前缀共用一套 9 位编号，
//! 启动时查重、建字符串 key 的模板

use std::collections::HashMap;
use std::sync::OnceLock;

use linkme::distributed_slice;
use minijinja::Environment;

use super::{LockKey, LockType};

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

/// `try_lock_i64` 默认实现转成字符串 key 时用的编号：全 9，999 段只有 hygiea 自己用，业务注册不会撞到
const I64_CODE: &str = "999999999";

#[distributed_slice(LOCK_REGISTRATIONS)]
static I64_REGISTRATION: LockRegistration = LockRegistration {
    code: I64_CODE,
    name: "hygiea::lock::i64",
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
    LockKey(Ok(format!("{I64_CODE}:{key}")))
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
        assert!(has("999999999", "hygiea::lock::i64"));
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
