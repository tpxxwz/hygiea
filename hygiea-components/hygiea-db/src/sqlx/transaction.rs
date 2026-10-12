//! 事务：[`transaction`] 用数据库默认的隔离级别

use hygiea_core::{Result, ResultExt, err};
use sqlx::{Database, Pool, Transaction};

use crate::RepoErr;

/// 在一个事务里执行 `f`：返回 `Ok` 就提交，返回 `Err` 就回滚，错误原样往外传。
/// 开启、提交事务本身失败时是 [`RepoErr::TransactionBeginFailed`]、[`RepoErr::TransactionCommitFailed`]，`op` 写进模板，原始错误挂 source。
///
/// sqlx 的语句要用 `&mut` 连接执行，闭包拿到的是 `&mut Transaction`，执行时写 `&mut **tx`：
///
/// ```ignore
/// sqlx::transaction(&pool, "save user", async |tx| {
///     sqlx::query("INSERT INTO users (name) VALUES ($1)")
///         .bind(name)
///         .execute(&mut **tx)
///         .await
///         .wrap_err(|| err!(RepoErr::InsertFailed, "insert user"))?;
///     Ok(())
/// })
/// .await?;
/// ```
pub async fn transaction<DB, T>(
    pool: &Pool<DB>,
    op: &str,
    f: impl AsyncFnOnce(&mut Transaction<'static, DB>) -> Result<T>,
) -> Result<T>
where
    DB: Database,
{
    let mut tx = pool
        .begin()
        .await
        .wrap_err(|| err!(RepoErr::TransactionBeginFailed, op))?;
    // 闭包返回 Err 时直接返回：tx 被 drop，sqlx 在 drop 里回滚
    let value = f(&mut tx).await?;
    tx.commit()
        .await
        .wrap_err(|| err!(RepoErr::TransactionCommitFailed, op))?;
    Ok(value)
}
