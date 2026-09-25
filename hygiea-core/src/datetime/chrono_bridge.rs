//! chrono 互转桥（feature `datetime-chrono`）。

use time::UtcDateTime;

use crate::{BaseErr, HyErr, ResultExt, err};

pub use chrono::{DateTime, Utc};

/// 框架 UTC 主类型是 time 的 `UtcDateTime`；持有 chrono `DateTime<Utc>` 的场景
/// （serde、第三方 chrono 生态）经此 trait 与主类型互转。
pub trait DateTimeUtcExt {
    fn to_utc_datetime(&self) -> Result<UtcDateTime, HyErr>;
    fn from_utc_datetime(datetime: UtcDateTime) -> Result<DateTime<Utc>, HyErr>;
}

impl DateTimeUtcExt for DateTime<Utc> {
    fn to_utc_datetime(&self) -> Result<UtcDateTime, HyErr> {
        let nanos =
            self.timestamp() as i128 * 1_000_000_000 + self.timestamp_subsec_nanos() as i128;
        UtcDateTime::from_unix_timestamp_nanos(nanos)
            .wrap_err(|| err!(BaseErr::DateError, "to_utc_datetime out of range"))
    }

    fn from_utc_datetime(datetime: UtcDateTime) -> Result<DateTime<Utc>, HyErr> {
        DateTime::from_timestamp(datetime.unix_timestamp(), datetime.nanosecond()).ok_or_else(
            || {
                err!(
                    BaseErr::DateError,
                    "from_utc_datetime: timestamp out of range"
                )
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    #[test]
    fn test_round_trip() {
        let chrono_dt = DateTime::from_timestamp(1_704_067_200, 789_123_456).unwrap();
        let utc_dt = chrono_dt.to_utc_datetime().unwrap();
        assert_eq!(utc_dt.year(), 2024);
        assert_eq!(utc_dt.unix_timestamp(), 1_704_067_200);
        assert_eq!(utc_dt.nanosecond(), 789_123_456);
        // 回程还原同一时刻
        assert_eq!(DateTime::from_utc_datetime(utc_dt).unwrap(), chrono_dt);

        // Unix 纪元之前
        let chrono_dt = DateTime::from_timestamp(-1, 1).unwrap();
        let utc_dt = chrono_dt.to_utc_datetime().unwrap();
        assert_eq!(utc_dt.unix_timestamp(), -1);
        assert_eq!(utc_dt.nanosecond(), 1);
        assert_eq!(DateTime::from_utc_datetime(utc_dt).unwrap(), chrono_dt);
    }

    #[test]
    fn test_to_utc_datetime_out_of_range() {
        // chrono 支持的年份范围比 time 大，超出 time 范围时报错
        let far = Utc.with_ymd_and_hms(100_000, 1, 1, 0, 0, 0).unwrap();
        assert!(far.to_utc_datetime().is_err());
    }
}
