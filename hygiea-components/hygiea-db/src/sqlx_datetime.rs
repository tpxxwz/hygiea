//! sqlx 的 UTC 时间列类型，feature `sqlx`
//!
//! sqlx 只认 time 的 `OffsetDateTime` 和 `PrimitiveDateTime`，不认 [`UtcDateTime`]，
//! 孤儿规则又不允许直接给它实现 sqlx 的 trait，所以包一层。读写时和 `OffsetDateTime` 互转，
//! 支持哪些数据库、对应什么列类型都跟着 `OffsetDateTime` 走（PostgreSQL 是 `timestamptz`）。
//!
//! ```ignore
//! let at: SqlxUtcDateTime = sqlx::query_scalar("SELECT create_at FROM record")
//!     .fetch_one(&pool)
//!     .await?;
//! sqlx::query("INSERT INTO record (create_at) VALUES ($1)")
//!     .bind(SqlxUtcDateTime(now_utc()))
//!     .execute(&pool)
//!     .await?;
//! ```

use hygiea_core::datetime::{OffsetDateTime, UtcDateTime, rfc3339_utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Database, Decode, Encode, Type};

/// sqlx 读写的 UTC 时间，对应的列类型同 `OffsetDateTime`。`.0` 或 `From` 取出 [`UtcDateTime`]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SqlxUtcDateTime(pub UtcDateTime);

impl From<UtcDateTime> for SqlxUtcDateTime {
    fn from(value: UtcDateTime) -> Self {
        Self(value)
    }
}

impl From<SqlxUtcDateTime> for UtcDateTime {
    fn from(value: SqlxUtcDateTime) -> Self {
        value.0
    }
}

/// 序列化成带 `Z` 的 RFC 3339，读的时候带任意 offset 都换算成 UTC，同 [`rfc3339_utc`]
impl Serialize for SqlxUtcDateTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        rfc3339_utc::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for SqlxUtcDateTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        rfc3339_utc::deserialize(deserializer).map(Self)
    }
}

impl<DB: Database> Type<DB> for SqlxUtcDateTime
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

impl<'q, DB: Database> Encode<'q, DB> for SqlxUtcDateTime
where
    OffsetDateTime: Encode<'q, DB>,
{
    fn encode_by_ref(&self, buf: &mut DB::ArgumentBuffer) -> Result<IsNull, BoxDynError> {
        <OffsetDateTime as Encode<'q, DB>>::encode(OffsetDateTime::from(self.0), buf)
    }
}

impl<'r, DB: Database> Decode<'r, DB> for SqlxUtcDateTime
where
    OffsetDateTime: Decode<'r, DB>,
{
    fn decode(value: DB::ValueRef<'r>) -> Result<Self, BoxDynError> {
        <OffsetDateTime as Decode<'r, DB>>::decode(value).map(|value| Self(value.to_utc()))
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use hygiea_core::datetime::HygieaDateTimeExt;
    use sqlx::SqlitePool;

    fn utc(s: &str) -> UtcDateTime {
        UtcDateTime::parse_ext_rfc3339(s).unwrap()
    }

    #[tokio::test]
    async fn round_trip_keeps_utc() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let at = SqlxUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let back: SqlxUtcDateTime = sqlx::query_scalar("SELECT ?")
            .bind(at)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(back, at);
    }

    #[tokio::test]
    async fn decode_with_offset_converts_to_utc() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let at: SqlxUtcDateTime = sqlx::query_scalar("SELECT '2026-10-05T20:57:24+08:00'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(at.0, utc("2026-10-05T12:57:24Z"));
    }
}

#[cfg(test)]
mod serde_tests {
    use super::*;
    use hygiea_core::datetime::HygieaDateTimeExt;

    fn utc(s: &str) -> UtcDateTime {
        UtcDateTime::parse_ext_rfc3339(s).unwrap()
    }

    #[test]
    fn serde_uses_rfc3339_utc() {
        let at = SqlxUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let json = serde_json::to_string(&at).unwrap();
        assert_eq!(json, r#""2026-10-05T12:57:24.719Z""#);
        let back: SqlxUtcDateTime =
            serde_json::from_str(r#""2026-10-05T20:57:24.719+08:00""#).unwrap();
        assert_eq!(back, at);
    }
}
