//! 分布式锁的错误码

use crate::hy_err;

/// 分布式锁的错误，底层原因挂在 source 上
#[derive(hy_err)]
#[err_code_internal_module_prefix = "003"]
pub enum LockErr {
    /// 加锁时后端出错（连不上、命令失败），不是"锁被别人占着"：占着的情况返回 `Ok(None)`
    #[error(err_code = "01", err_tpl = "Acquire lock failed: {{ key }}")]
    AcquireFailed,
    /// 释放锁时后端出错。锁最终还是会被后端释放（事务结束、连接断开、TTL 过期）
    #[error(err_code = "02", err_tpl = "Release lock failed: {{ key }}")]
    ReleaseFailed,
    /// 字符串 key 的模板渲染失败：`lock_key!` 的 `{ .. }` 写法里 key 用了表达式，编译期查不了，运行时缺了变量
    #[error(err_code = "03", err_tpl = "Invalid lock key: {{ cause }}")]
    InvalidKey,
    /// 后端不支持这个操作：后端不想提供的方法照常实现（覆盖默认实现，或者实现必须的 `try_lock`），
    /// 直接返回它，`op` 写后端和方法，比如 `"redis: try_lock_i32"`
    #[error(err_code = "04", err_tpl = "Unsupported lock operation: {{ op }}")]
    Unsupported,
}
