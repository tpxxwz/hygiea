//! hygiea 内置保留的编号，常开（不跟着哪个 feature 走），各模块、组件共用

/// 内置保留的锁编号的数字，只写这一遍；下面的数值和字符串都从它来
macro_rules! lock_type {
    () => {
        999999999
    };
}

/// 内置保留的锁编号：全 9，999 段只有 hygiea 自己用，业务注册不到（`lock` 模块的注册表里占着这一条）。用在：
/// - `lock` 模块里 `try_lock_i64` 默认实现转成的字符串 key 的前缀：`999999999:<数字>`
/// - hygiea 组件自己用的 PG advisory lock 的第一个数：`(999999999, Lock 的值)`，在 `(int, int)` 空间，
///   和业务 `try_lock_i64` 的 bigint 空间分开，业务的锁类型又注册不到 999 段，所以不会撞
pub const LOCK_TYPE: i32 = lock_type!();

/// [`LOCK_TYPE`] 的字符串形式，锁注册表的编号是字符串（编译期就要有）
pub const LOCK_CODE: &str = concat!(lock_type!());

/// hygiea 组件自己用的 PG advisory lock：`(LOCK_TYPE, 这里的值)`。加新的往后排，已有的值不要改
/// （改了，还在跑的旧版本和新版本拿的就不是同一把锁）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Lock {
    /// hygiea-db 建动态配置的表时串行化（多个实例同时启动只让一个建）
    LiveConfigDdl = 1,
    /// hygiea-db 建锁表时串行化
    LockTableDdl = 2,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_type_matches_code() {
        assert_eq!(LOCK_TYPE, 999_999_999);
        assert_eq!(LOCK_TYPE.to_string(), LOCK_CODE);
        assert_eq!(
            (Lock::LiveConfigDdl as i32, Lock::LockTableDdl as i32),
            (1, 2)
        );
    }
}
