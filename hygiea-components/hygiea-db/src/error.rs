//! 数据库操作的错误。和 [`BaseErr`](hygiea_core::BaseErr) 共用项目前缀 999，内部模块前缀是 100
//!
//! 按调用方做的操作分变体，不按底层原因分：唯一约束冲突、连接超时、类型转换失败这些具体原因
//! 都是 source 上的原始错误（sqlx / SeaORM 的），打 `{:#}` 能看到。变体码按十位分组：
//!
//! | 码 | 组 | 谁用 |
//! |---|---|---|
//! | 0x | 事务 | 各框架的 `transaction` 系列函数 |
//! | 1x | 执行语句失败：10 兜底（原生 SQL、DDL），11-14 增删改查 | `db!` 按语句类型自动选，`.db()` 一律 10；`seaorm::find_page` 是 14，`op` 里写函数名；直接用 sqlx 时自己转 |
//! | 2x | 动态配置 | 连接池的 `live_config` / `live_config_all`，包括第一次绑定时的建表检查 |
//! | 3x | 分布式锁 | 锁表（feature `lock-table`）的建表检查；加锁、释放失败是 hygiea-core 的 `LockErr` |
//! | 9x | 参数不对 | 没执行 SQL 就拦下来的，没有 source |
//!
//! 调用方自己写的语句用 1x 的，`op` 写明是哪一步：
//!
//! ```ignore
//! let user = user::Entity::find_by_id(id)
//!     .one(db)
//!     .await
//!     .wrap_err(|| err!(RepoErr::QueryFailed, "find user"))?;
//! ```
//!
//! 「没查到」不是数据库错误：查询返回 `None` / 空列表，要不要当成错误由业务决定，用业务自己的错误码。
//! 这里的错误项目前缀是 999，HTTP 组件对外只显示 "System Error"，原错误记在服务端日志里

use hygiea_core::hy_err;

/// 数据库操作失败。模板里的 `op` 是哪一步，原始错误挂在 source 上；9x 没有 source，原因在模板里
#[derive(hy_err)]
#[err_code_internal_module_prefix = "100"]
pub enum RepoErr {
    // ---- 0x 事务：开启、提交本身失败（不是事务里的语句失败）。回滚在 drop 里做，不报错 ----
    /// 开启事务失败
    #[error(err_code = "01", err_tpl = "Db transaction begin failed: {{ op }}")]
    TransactionBeginFailed,
    /// 提交事务失败
    #[error(err_code = "02", err_tpl = "Db transaction commit failed: {{ op }}")]
    TransactionCommitFailed,

    // ---- 1x 执行语句失败：10 是兜底，11-14 按增删改查细分。`op` 写是哪一步 ----
    /// 执行原生 SQL、DDL 等不属于增删改查的语句失败
    #[error(err_code = "10", err_tpl = "Db exec failed: {{ op }}")]
    ExecFailed,
    /// 插入失败，比如主键、唯一约束冲突
    #[error(err_code = "11", err_tpl = "Db insert failed: {{ op }}")]
    InsertFailed,
    /// 删除失败
    #[error(err_code = "12", err_tpl = "Db delete failed: {{ op }}")]
    DeleteFailed,
    /// 更新失败
    #[error(err_code = "13", err_tpl = "Db update failed: {{ op }}")]
    UpdateFailed,
    /// 查询失败
    #[error(err_code = "14", err_tpl = "Db query failed: {{ op }}")]
    QueryFailed,

    // ---- 2x 动态配置：连接池的 live_config / live_config_all。查询、写入失败用 1x ----
    /// 默认值序列化不出 JSON 对象（`bind` 的 `T`、`bind_all` 第一层的每个字段都要是对象）
    #[error(err_code = "21", err_tpl = "Db live config defaults invalid: {{ ns }}")]
    LiveConfigDefaultsInvalid,
    /// 表里的值转不成 struct，比如类型不对
    #[error(err_code = "22", err_tpl = "Db live config decode failed: {{ ns }}")]
    LiveConfigDecodeFailed,
    /// 后台任务已经退出（连接池关了）
    #[error(err_code = "23", err_tpl = "Db live config closed")]
    LiveConfigClosed,
    /// 表缺了、也建不了，多半是没有 DDL 权限。要执行的 DDL 打在日志里
    #[error(
        err_code = "24",
        err_tpl = "Db live config tables create failed: {{ table }}"
    )]
    LiveConfigCreateFailed,
    /// 表结构和代码对不上，或者有同名的、不是 hygiea 建的函数、触发器。不动它们、拒绝启动，
    /// 对不上的地方和要执行的 DDL 打在日志里，手动调整（或者换前缀）后再启动
    #[error(
        err_code = "25",
        err_tpl = "Db live config schema mismatch: {{ table }}"
    )]
    LiveConfigSchemaMismatch,
    /// 这个连接池没开动态配置（`[..live_config] enabled = true`）就调了 `live_config` / `live_config_all`
    #[error(err_code = "26", err_tpl = "Db live config not enabled on this pool")]
    LiveConfigDisabled,

    // ---- 3x 分布式锁：锁表的建表检查。加锁、释放失败用 hygiea-core 的 LockErr ----
    /// 锁表（`{app_name}_lock`）建不了，多半是没有 DDL 权限；要手动执行的 DDL 打在日志里
    #[error(err_code = "31", err_tpl = "Db lock table create failed: {{ table }}")]
    LockTableCreateFailed,
    /// 库里已有同名的锁表，但列和代码对不上：不动它，拒绝启动；哪里不对和 DDL 打在日志里
    #[error(
        err_code = "32",
        err_tpl = "Db lock table schema mismatch: {{ table }}"
    )]
    LockTableSchemaMismatch,

    // ---- 9x 参数不对：没执行 SQL 就拦下来 ----
    /// 分页参数超出范围：页号 × 页大小（OFFSET）超过 `i64::MAX`。[`PageRequest::new`](crate::PageRequest::new) 和反序列化时报
    #[error(
        err_code = "91",
        err_tpl = "Db page out of range: page {{ page }}, page_size {{ page_size }}"
    )]
    PageOutOfRange,
}
