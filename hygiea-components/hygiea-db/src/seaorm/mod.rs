//! SeaORM 专用的部分，feature `seaorm`：各数据库共用的 [`transaction`]、[`HyUtcDateTime`](crate::HyUtcDateTime) 的 trait 实现；
//! 每种数据库一个文件（PostgreSQL 连接组件），再导出到 crate 根

#[cfg(feature = "postgres")]
pub(crate) mod postgres;

use hygiea_core::datetime::OffsetDateTime;
use hygiea_core::{Result, ResultExt, err};
use sea_orm::sea_query::{ArrayType, ColumnType, Nullable, ValueType, ValueTypeErr};
use sea_orm::{
    ActiveValue, ColIdx, IntoActiveValue, QueryResult, TransactionSession, TransactionTrait,
    TryGetError, TryGetable, Value,
};

use crate::{HyUtcDateTime, RepoErr};

/// 在一个事务里执行 `f`：返回 `Ok` 就提交，返回 `Err` 就回滚，错误原样往外传。
/// 开启、提交事务本身失败时是 [`RepoErr::TransactionFailed`]，`op` 写进模板，原始错误挂 source。
///
/// `db` 是连接池时开新事务，是事务时开嵌套事务（SeaORM 用 savepoint）。闭包是 async 闭包，
/// 可以直接借用外面的变量：
///
/// ```ignore
/// seaorm::transaction(&db, "save user", async |txn| {
///     user::Entity::insert(am).exec(txn).await.wrap_err(|| err!(RepoErr::InsertFailed, "insert user"))?;
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
    let txn = db
        .begin()
        .await
        .wrap_err(|| err!(RepoErr::TransactionFailed, op))?;
    // 闭包返回 Err 时直接返回：txn 被 drop，SeaORM 在 drop 里回滚
    let value = f(&txn).await?;
    txn.commit()
        .await
        .wrap_err(|| err!(RepoErr::TransactionFailed, op))?;
    Ok(value)
}

// ---- HyUtcDateTime 的 SeaORM trait 实现：经 OffsetDateTime 转换 ----

impl From<HyUtcDateTime> for Value {
    fn from(value: HyUtcDateTime) -> Self {
        OffsetDateTime::from(value.0).into()
    }
}

impl TryGetable for HyUtcDateTime {
    fn try_get_by<I: ColIdx>(res: &QueryResult, idx: I) -> Result<Self, TryGetError> {
        OffsetDateTime::try_get_by(res, idx).map(|value| Self(value.to_utc()))
    }
}

impl ValueType for HyUtcDateTime {
    fn try_from(v: Value) -> Result<Self, ValueTypeErr> {
        <OffsetDateTime as ValueType>::try_from(v).map(|value| Self(value.to_utc()))
    }

    fn type_name() -> String {
        "HyUtcDateTime".to_owned()
    }

    fn array_type() -> ArrayType {
        <OffsetDateTime as ValueType>::array_type()
    }

    fn column_type() -> ColumnType {
        <OffsetDateTime as ValueType>::column_type()
    }
}

impl Nullable for HyUtcDateTime {
    fn null() -> Value {
        <OffsetDateTime as Nullable>::null()
    }
}

impl IntoActiveValue<HyUtcDateTime> for HyUtcDateTime {
    fn into_active_value(self) -> ActiveValue<HyUtcDateTime> {
        ActiveValue::Set(self)
    }
}

#[cfg(test)]
mod tests {
    use crate::HyUtcDateTime;
    use hygiea_core::datetime::{HygieaDateTimeExt, UtcDateTime};

    fn utc(s: &str) -> UtcDateTime {
        UtcDateTime::parse_ext_rfc3339(s).unwrap()
    }
    use sea_orm::Value;
    use sea_orm::sea_query::{ColumnType, Nullable, ValueType};

    #[test]
    fn value_round_trip_keeps_utc() {
        let at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let value = Value::from(at);
        assert!(matches!(value, Value::TimeDateTimeWithTimeZone(Some(_))));
        assert_eq!(<HyUtcDateTime as ValueType>::try_from(value).unwrap(), at);
    }

    #[test]
    fn value_with_offset_converts_to_utc() {
        let shanghai = utc("2026-10-05T12:57:24Z")
            .to_offset(hygiea_core::datetime::time::UtcOffset::from_hms(8, 0, 0).unwrap());
        let at = <HyUtcDateTime as ValueType>::try_from(Value::from(shanghai)).unwrap();
        assert_eq!(at.0, utc("2026-10-05T12:57:24Z"));
    }

    #[test]
    fn column_type_is_timestamptz() {
        assert_eq!(
            <HyUtcDateTime as ValueType>::column_type(),
            ColumnType::TimestampWithTimeZone
        );
        assert_eq!(
            <HyUtcDateTime as Nullable>::null(),
            Value::TimeDateTimeWithTimeZone(None)
        );
    }
}
