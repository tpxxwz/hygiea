//! 事务：[`transaction`] 用数据库默认的隔离级别，[`transaction_snapshot`] 是一致性只读，
//! [`transaction_with_config`] 自己指定隔离级别和读写模式

use hygiea_core::{Result, ResultExt, err};
use sea_orm::{AccessMode, IsolationLevel, TransactionSession, TransactionTrait};

use crate::RepoErr;

/// 在一个事务里执行 `f`：返回 `Ok` 就提交，返回 `Err` 就回滚，错误原样往外传。
/// 开启、提交事务本身失败时是 [`RepoErr::TransactionBeginFailed`]、[`RepoErr::TransactionCommitFailed`]，`op` 写进模板，原始错误挂 source。
///
/// `db` 是连接池时开新事务，是事务时开嵌套事务（SeaORM 用 savepoint）。闭包是 async 闭包，
/// 可以直接借用外面的变量：
///
/// ```ignore
/// seaorm::transaction(&db, "save user", async |txn| {
///     db!(user::Entity::insert(am).exec(txn))?;
///     Ok(())
/// })
/// .await?;
/// ```
pub async fn transaction<C, T>(
    db: &C,
    op: &str,
    f: impl AsyncFnOnce(&C::Transaction) -> Result<T>,
) -> Result<T>
where
    C: TransactionTrait,
{
    transaction_with_config(db, None, None, op, f).await
}

/// 一致性只读：REPEATABLE READ + READ ONLY 开事务执行 `f`，其余同 [`transaction`]。
/// 一次读多张表拼一个完整对象时用：整个事务读同一个快照，中途别人提交的改动看不到，
/// 不会一半旧一半新；事务里写数据直接报错。
///
/// 语义是 SeaORM 在各库上的：PostgreSQL、MySQL 的普通 SELECT 都读快照；SQLite 下 SeaORM 忽略这两个设置
/// （只打 warn），不过 SQLite 的读事务本身就是快照读。`db` 已经是事务时（嵌套）PostgreSQL 不能中途改隔离级别，
/// 用 [`transaction`]
///
/// ```ignore
/// seaorm::transaction_snapshot(&db, "read order", async |txn| {
///     let order = find_order(txn, id).await?;
///     let items = find_items(txn, id).await?;
///     Ok((order, items))
/// })
/// .await?;
/// ```
pub async fn transaction_snapshot<C, T>(
    db: &C,
    op: &str,
    f: impl AsyncFnOnce(&C::Transaction) -> Result<T>,
) -> Result<T>
where
    C: TransactionTrait,
{
    transaction_with_config(
        db,
        Some(IsolationLevel::RepeatableRead),
        Some(AccessMode::ReadOnly),
        op,
        f,
    )
    .await
}

/// 同 [`transaction`]，开事务时指定隔离级别和读写模式，`None` 用数据库默认。只对新事务有意义：
/// `db` 已经是事务时（嵌套，savepoint）PostgreSQL 不能中途改隔离级别，传 `None`。
/// 比如一次读多张表要看到同一个快照（PostgreSQL 默认的 READ COMMITTED 每条语句各取快照）：
///
/// ```ignore
/// seaorm::transaction_with_config(
///     &db,
///     Some(IsolationLevel::RepeatableRead),
///     Some(AccessMode::ReadOnly),
///     "read order",
///     async |txn| {
///         let order = db!(order::Entity::find_by_id(id).one(txn))?;
///         let items = db!(item::Entity::find().filter(item::Column::OrderId.eq(id)).all(txn))?;
///         Ok((order, items))
///     },
/// )
/// .await?;
/// ```
pub async fn transaction_with_config<C, T>(
    db: &C,
    isolation_level: Option<IsolationLevel>,
    access_mode: Option<AccessMode>,
    op: &str,
    f: impl AsyncFnOnce(&C::Transaction) -> Result<T>,
) -> Result<T>
where
    C: TransactionTrait,
{
    let txn = db
        .begin_with_config(isolation_level, access_mode)
        .await
        .wrap_err(|| err!(RepoErr::TransactionBeginFailed, op))?;
    // 闭包返回 Err 时直接返回：txn 被 drop，SeaORM 在 drop 里回滚
    let value = f(&txn).await?;
    txn.commit()
        .await
        .wrap_err(|| err!(RepoErr::TransactionCommitFailed, op))?;
    Ok(value)
}
