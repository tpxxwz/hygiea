//! 锁的 key：
//! - 整数 key [`LockKeyI32`] / [`LockKeyI64`]：能 `Into<i32>` / `Into<i64>` 的类型都能传（和标准库一致），另外多了 `u32` / `u64`，按位转
//! - 锁类型 [`LockType`]（`try_lock_i32` 的 `key1`）、字符串 key [`LockKey`]：只能由注册过的 derive 生成，见 registry.rs

use std::num::{NonZeroI32, NonZeroI64};

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

    /// 把字符串 key 哈希成 i64，交给 `*_i64` 的锁：PG 上是 advisory lock，不用锁表，换性能。
    ///
    /// 这是有损的降级，要使用方自己选：不同 key 可能撞到同一个值（多锁，不会少锁），也和直接传数字的
    /// `try_lock_i64` 共用同一个空间。算法固定（xxh3-64，seed 0），跨进程、跨机器、跨版本结果一致。
    ///
    /// ```ignore
    /// db.try_lock_i64(lock_key!(OrderLockKey::Export, user_name).hashed_i64()?).await?;
    /// ```
    pub fn hashed_i64(&self) -> crate::Result<LockKeyI64> {
        let hash = xxhash_rust::xxh3::xxh3_64(self.as_str()?.as_bytes());
        Ok(LockKeyI64::from(hash))
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 算法固定：同一个 key 的结果写死在这里，换了算法或 seed 会失败
    #[test]
    fn hashed_i64_is_stable() {
        let key = |s: &str| LockKey(Ok(s.to_string()));
        let hashed = key("001020002:settle:2026-10-13")
            .hashed_i64()
            .unwrap()
            .get();
        assert_eq!(hashed, HASHED_SETTLE);
        assert_ne!(
            hashed,
            key("001020002:settle:2026-10-14")
                .hashed_i64()
                .unwrap()
                .get()
        );
        let invalid = LockKey(Err("missing id".to_string()))
            .hashed_i64()
            .unwrap_err();
        assert!(invalid.is(crate::lock::LockErr::InvalidKey));
    }

    const HASHED_SETTLE: i64 = 4973850726513738739;

    #[test]
    fn pair_is_one_to_one() {
        let pair = |a: i32, b: i32| LockKeyI64::from_pair(a.into(), b.into()).get();
        assert_eq!(pair(0, 42), 42);
        assert_eq!(pair(1, -1), (1i64 << 32) | 0xFFFF_FFFF);
        assert_eq!(pair(-1, 0), -1i64 << 32);
        assert_ne!(pair(0, -1), pair(-1, -1));
    }
}
