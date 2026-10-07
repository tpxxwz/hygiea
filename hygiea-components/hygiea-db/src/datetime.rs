//! 数据库里的 UTC 时间列类型 [`HyUtcDateTime`]，开 `sqlx` 或 `seaorm` 就有，和数据库无关
//!
//! sqlx 和 SeaORM 只认 time 的 `OffsetDateTime` 和 `PrimitiveDateTime`，不认 [`UtcDateTime`]，
//! 孤儿规则又不允许直接给它实现框架的 trait，所以包一层。同一个类型按 feature 实现两边的 trait，
//! 两个框架同时开也是同一个类型。读写时和 `OffsetDateTime` 互转，支持哪些数据库、对应什么列类型
//! 都跟着 `OffsetDateTime` 走（PostgreSQL 是 `timestamptz`）。
//!
//! ```ignore
//! // sqlx
//! let at: HyUtcDateTime = sqlx::query_scalar("SELECT create_at FROM record")
//!     .fetch_one(&pool)
//!     .await?;
//! sqlx::query("INSERT INTO record (create_at) VALUES ($1)")
//!     .bind(HyUtcDateTime(now_utc()))
//!     .execute(&pool)
//!     .await?;
//!
//! // SeaORM
//! #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
//! #[sea_orm(table_name = "record")]
//! pub struct Model {
//!     #[sea_orm(primary_key)]
//!     pub id: i64,
//!     pub create_at: HyUtcDateTime,
//!     pub delete_at: Option<HyUtcDateTime>,
//! }
//! ```

use hygiea_core::datetime::{UtcDateTime, rfc3339_utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 数据库里的 UTC 时间，列类型同 `OffsetDateTime`。`.0` 或 `From` 取出 [`UtcDateTime`]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HyUtcDateTime(pub UtcDateTime);

impl From<UtcDateTime> for HyUtcDateTime {
    fn from(value: UtcDateTime) -> Self {
        Self(value)
    }
}

impl From<HyUtcDateTime> for UtcDateTime {
    fn from(value: HyUtcDateTime) -> Self {
        value.0
    }
}

/// 序列化成带 `Z` 的 RFC 3339，读的时候带任意 offset 都换算成 UTC，同 [`rfc3339_utc`]
impl Serialize for HyUtcDateTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        rfc3339_utc::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for HyUtcDateTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        rfc3339_utc::deserialize(deserializer).map(Self)
    }
}

/// sqlx：feature `sqlx`
#[cfg(feature = "sqlx")]
mod sqlx_impl {
    use super::HyUtcDateTime;
    use hygiea_core::datetime::OffsetDateTime;
    use sqlx::encode::IsNull;
    use sqlx::error::BoxDynError;
    use sqlx::{Database, Decode, Encode, Type};

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
}

/// SeaORM：feature `seaorm`
#[cfg(feature = "seaorm")]
mod seaorm_impl {
    use super::HyUtcDateTime;
    use hygiea_core::datetime::OffsetDateTime;
    use sea_orm::sea_query::{ArrayType, ColumnType, Nullable, ValueType, ValueTypeErr};
    use sea_orm::{
        ActiveValue, ColIdx, IntoActiveValue, QueryResult, TryGetError, TryGetable, Value,
    };

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use hygiea_core::datetime::HygieaDateTimeExt;

    fn utc(s: &str) -> UtcDateTime {
        UtcDateTime::parse_ext_rfc3339(s).unwrap()
    }

    #[test]
    fn serde_uses_rfc3339_utc() {
        let at = HyUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let json = serde_json::to_string(&at).unwrap();
        assert_eq!(json, r#""2026-10-05T12:57:24.719Z""#);
        let back: HyUtcDateTime =
            serde_json::from_str(r#""2026-10-05T20:57:24.719+08:00""#).unwrap();
        assert_eq!(back, at);
    }

    #[cfg(all(feature = "sqlx", feature = "sqlite"))]
    mod sqlx_tests {
        use super::*;
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

    #[cfg(feature = "seaorm")]
    mod seaorm_tests {
        use super::*;
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
}
