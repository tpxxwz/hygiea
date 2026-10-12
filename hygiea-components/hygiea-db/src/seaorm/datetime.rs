//! [`HyUtcDateTime`] 的 SeaORM trait 实现：经 `OffsetDateTime` 转换

use hygiea_core::Result;
use hygiea_core::datetime::OffsetDateTime;
use sea_orm::sea_query::{ArrayType, ColumnType, Nullable, ValueType, ValueTypeErr};
use sea_orm::{ActiveValue, ColIdx, IntoActiveValue, QueryResult, TryGetError, TryGetable, Value};

use crate::HyUtcDateTime;

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
