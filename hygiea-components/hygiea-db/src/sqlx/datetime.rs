//! [`HyUtcDateTime`] 的 sqlx trait 实现：经 `OffsetDateTime` 转换

use hygiea_core::datetime::OffsetDateTime;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Database, Decode, Encode, Type};

use crate::HyUtcDateTime;

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
