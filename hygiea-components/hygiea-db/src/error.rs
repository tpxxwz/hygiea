//! 数据库操作的错误。和 [`BaseErr`](hygiea_core::BaseErr) 共用项目前缀 999，内部模块前缀是 100
//!
//! 按调用方做的操作分变体，不按底层原因分：唯一约束冲突、连接超时、类型转换失败这些具体原因
//! 都是 source 上的原始错误（sqlx / SeaORM 的），打 `{:#}` 能看到。调用方自己转，`op` 写明是哪一步：
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

/// 数据库操作失败。模板里只有 `op`（调用方写的是哪一步），原始错误挂在 source 上
#[derive(hy_err)]
#[err_code_internal_module_prefix = "100"]
pub enum RepoErr {
    /// 开启、提交、回滚事务本身失败（不是事务里的语句失败）。各框架模块的 `transaction` 函数会用到
    #[error(err_code = "01", err_tpl = "Db transaction failed: {{ op }}")]
    TransactionFailed,
    /// 查询失败
    #[error(err_code = "02", err_tpl = "Db query failed: {{ op }}")]
    QueryFailed,
    /// 插入失败，比如主键、唯一约束冲突
    #[error(err_code = "03", err_tpl = "Db insert failed: {{ op }}")]
    InsertFailed,
    /// 更新失败
    #[error(err_code = "04", err_tpl = "Db update failed: {{ op }}")]
    UpdateFailed,
    /// 删除失败
    #[error(err_code = "05", err_tpl = "Db delete failed: {{ op }}")]
    DeleteFailed,
    /// 执行原生 SQL、DDL 等不属于增删改查的语句失败
    #[error(err_code = "06", err_tpl = "Db exec failed: {{ op }}")]
    ExecFailed,
}
