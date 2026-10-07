//! sqlx 专用的部分，feature `sqlx`：两种数据库共用的 [`transaction`]、[`HyUtcDateTime`](crate::HyUtcDateTime) 的 trait 实现；
//! 每种数据库一个文件（PostgreSQL、SQLite 连接池组件），再导出到 crate 根

#[cfg(feature = "postgres")]
pub(crate) mod postgres;
#[cfg(feature = "sqlite")]
pub(crate) mod sqlite;

use hygiea_core::datetime::OffsetDateTime;
use hygiea_core::{Result, ResultExt, err};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Database, Decode, Encode, Pool, Transaction, Type};

use crate::{HyUtcDateTime, RepoErr};

/// 在一个事务里执行 `f`：返回 `Ok` 就提交，返回 `Err` 就回滚，错误原样往外传。
/// 开启、提交事务本身失败时是 [`RepoErr::TransactionFailed`]，`op` 写进模板，原始错误挂 source。
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
        .wrap_err(|| err!(RepoErr::TransactionFailed, op))?;
    // 闭包返回 Err 时直接返回：tx 被 drop，sqlx 在 drop 里回滚
    let value = f(&mut tx).await?;
    tx.commit()
        .await
        .wrap_err(|| err!(RepoErr::TransactionFailed, op))?;
    Ok(value)
}

// ---- HyUtcDateTime 的 sqlx trait 实现：经 OffsetDateTime 转换 ----

impl<DB: Database> Type<DB> for HyUtcDateTime
where
    OffsetDateTime: Type<DB>,
{
    fn type_info() -> DB::TypeInfo {
        <OffsetDateTime as Type<DB>>::type_info()
    }

    fn compatible(ty: &DB::TypeInfo) -> bool {
        <OffsetDateTime as Type<DB>>::compatible(ty)
    }
}

impl<'q, DB: Database> Encode<'q, DB> for HyUtcDateTime
where
    OffsetDateTime: Encode<'q, DB>,
{
    fn encode_by_ref(&self, buf: &mut DB::ArgumentBuffer) -> Result<IsNull, BoxDynError> {
        <OffsetDateTime as Encode<'q, DB>>::encode(OffsetDateTime::from(self.0), buf)
    }
}

impl<'r, DB: Database> Decode<'r, DB> for HyUtcDateTime
where
    OffsetDateTime: Decode<'r, DB>,
{
    fn decode(value: DB::ValueRef<'r>) -> Result<Self, BoxDynError> {
        <OffsetDateTime as Decode<'r, DB>>::decode(value).map(|value| Self(value.to_utc()))
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::HyUtcDateTime;
    use hygiea_core::datetime::{HygieaDateTimeExt, UtcDateTime};

    fn utc(s: &str) -> UtcDateTime {
        UtcDateTime::parse_ext_rfc3339(s).unwrap()
    }
    use sqlx::SqlitePool;

    #[tokio::test]
    async fn round_trip_keeps_utc() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let back: HyUtcDateTime = sqlx::query_scalar("SELECT ?")
            .bind(at)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(back, at);
    }

    #[tokio::test]
    async fn decode_with_offset_converts_to_utc() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let at: HyUtcDateTime = sqlx::query_scalar("SELECT '2026-10-05T20:57:24+08:00'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(at.0, utc("2026-10-05T12:57:24Z"));
    }
}
