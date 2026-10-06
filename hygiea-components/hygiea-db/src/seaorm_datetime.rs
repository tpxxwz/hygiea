//! SeaORM 的 UTC 时间列类型，feature `seaorm`
//!
//! SeaORM 只认 time 的 `OffsetDateTime`（`timestamptz`）和 `PrimitiveDateTime`（`timestamp`），
//! 不认 [`UtcDateTime`]，孤儿规则又不允许直接给它实现 SeaORM 的 trait，所以包一层。
//! 读写时和 `OffsetDateTime` 互转，库里的列要用带时区的类型（PostgreSQL 的 `timestamptz`）。
//!
//! ```ignore
//! #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
//! #[sea_orm(table_name = "record")]
//! pub struct Model {
//!     #[sea_orm(primary_key)]
//!     pub id: i64,
//!     pub create_at: SeaOrmUtcDateTime,
//!     pub delete_at: Option<SeaOrmUtcDateTime>,
//! }
//! ```

use hygiea_core::datetime::{OffsetDateTime, UtcDateTime, rfc3339_utc};
use sea_orm::sea_query::{ArrayType, ColumnType, Nullable, ValueType, ValueTypeErr};
use sea_orm::{ActiveValue, ColIdx, IntoActiveValue, QueryResult, TryGetError, TryGetable, Value};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// SeaORM 实体里的 UTC 时间字段，对应 `timestamptz` 列。`.0` 或 `From` 取出 [`UtcDateTime`]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeaOrmUtcDateTime(pub UtcDateTime);

impl From<UtcDateTime> for SeaOrmUtcDateTime {
    fn from(value: UtcDateTime) -> Self {
        Self(value)
    }
}

impl From<SeaOrmUtcDateTime> for UtcDateTime {
    fn from(value: SeaOrmUtcDateTime) -> Self {
        value.0
    }
}

/// 序列化成带 `Z` 的 RFC 3339，读的时候带任意 offset 都换算成 UTC，同 [`rfc3339_utc`]
impl Serialize for SeaOrmUtcDateTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        rfc3339_utc::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for SeaOrmUtcDateTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        rfc3339_utc::deserialize(deserializer).map(Self)
    }
}

impl From<SeaOrmUtcDateTime> for Value {
    fn from(value: SeaOrmUtcDateTime) -> Self {
        OffsetDateTime::from(value.0).into()
    }
}

impl TryGetable for SeaOrmUtcDateTime {
    fn try_get_by<I: ColIdx>(res: &QueryResult, idx: I) -> Result<Self, TryGetError> {
        OffsetDateTime::try_get_by(res, idx).map(|value| Self(value.to_utc()))
    }
}

impl ValueType for SeaOrmUtcDateTime {
    fn try_from(v: Value) -> Result<Self, ValueTypeErr> {
        <OffsetDateTime as ValueType>::try_from(v).map(|value| Self(value.to_utc()))
    }

    fn type_name() -> String {
        "SeaOrmUtcDateTime".to_owned()
    }

    fn array_type() -> ArrayType {
        <OffsetDateTime as ValueType>::array_type()
    }

    fn column_type() -> ColumnType {
        <OffsetDateTime as ValueType>::column_type()
    }
}

impl Nullable for SeaOrmUtcDateTime {
    fn null() -> Value {
        <OffsetDateTime as Nullable>::null()
    }
}

impl IntoActiveValue<SeaOrmUtcDateTime> for SeaOrmUtcDateTime {
    fn into_active_value(self) -> ActiveValue<SeaOrmUtcDateTime> {
        ActiveValue::Set(self)
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
    fn value_round_trip_keeps_utc() {
        let at = SeaOrmUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let value = Value::from(at);
        assert!(matches!(value, Value::TimeDateTimeWithTimeZone(Some(_))));
        assert_eq!(
            <SeaOrmUtcDateTime as ValueType>::try_from(value).unwrap(),
            at
        );
    }

    #[test]
    fn value_with_offset_converts_to_utc() {
        let shanghai = utc("2026-10-05T12:57:24Z")
            .to_offset(hygiea_core::datetime::time::UtcOffset::from_hms(8, 0, 0).unwrap());
        let at = <SeaOrmUtcDateTime as ValueType>::try_from(Value::from(shanghai)).unwrap();
        assert_eq!(at.0, utc("2026-10-05T12:57:24Z"));
    }

    #[test]
    fn column_type_is_timestamptz() {
        assert_eq!(
            <SeaOrmUtcDateTime as ValueType>::column_type(),
            ColumnType::TimestampWithTimeZone
        );
        assert_eq!(
            <SeaOrmUtcDateTime as Nullable>::null(),
            Value::TimeDateTimeWithTimeZone(None)
        );
    }

    #[test]
    fn serde_uses_rfc3339_utc() {
        let at = SeaOrmUtcDateTime(utc("2026-10-05T12:57:24.719Z"));
        let json = serde_json::to_string(&at).unwrap();
        assert_eq!(json, r#""2026-10-05T12:57:24.719Z""#);
        let back: SeaOrmUtcDateTime =
            serde_json::from_str(r#""2026-10-05T20:57:24.719+08:00""#).unwrap();
        assert_eq!(back, at);
    }
}
