//! 两种类型共有的扩展（时间戳、RFC 3339、格式化、平移）和 `UtcDateTime` 的日历边界。

use time::format_description::well_known::Rfc3339;
use time::{Duration, Month, OffsetDateTime, Time, UtcDateTime};

use super::{DateTimeFormattable, DateTimeFormatter};
use crate::{BaseErr, HyErr, ResultExt, err};

/// UTC/绝对时间相关的通用日期时间扩展。
///
/// `OffsetDateTime` 与 `UtcDateTime` 都实现本 trait；返回日期时间的方法保持实现类型。
pub trait HygieaDateTimeExt: Sized + DateTimeFormattable {
    /// 从 Unix 毫秒时间戳创建日期时间。
    fn from_millis(ms: i64) -> Result<Self, HyErr>;
    /// 从 Unix 秒时间戳创建日期时间。
    fn from_secs(s: i64) -> Result<Self, HyErr>;

    /// 解析携带 offset 的 RFC 3339；时区/offset 语义由实现类型决定。
    fn parse_ext_rfc3339(s: &str) -> Result<Self, HyErr>;
    /// 按实现类型自身的时区/offset 语义输出 RFC 3339。
    fn format_ext_rfc3339(&self) -> Result<String, HyErr> {
        self.format_with(&Rfc3339)
            .wrap_err(|| err!(BaseErr::DateError, "format RFC 3339 datetime failed"))
    }
    /// 按指定格式输出日期时间，钟面时间按值自身的 offset 算。
    ///
    /// 配不带 offset 的格式时，字符串里没有 offset：`OffsetDateTime` 不一定是系统时区（比如 [`now()`](super::now)
    /// 是 UTC），要本地时间用 `format_ext_local`。
    fn format_ext(&self, formatter: impl Into<DateTimeFormatter>) -> Result<String, HyErr> {
        let result = match formatter.into() {
            DateTimeFormatter::WithOffset(formatter) => self.format_with(formatter.description()),
            DateTimeFormatter::WithoutOffset(formatter) => {
                self.format_with(formatter.description())
            }
        };
        result.wrap_err(|| err!(BaseErr::DateError, "format datetime failed"))
    }

    /// 在绝对时间线上平移指定时长，`OffsetDateTime` 保留自身的 offset。越界返回 `DateError`
    fn shift(&self, duration: Duration) -> Result<Self, HyErr>;
}

/// `UtcDateTime` 专有的 UTC 日历边界扩展。
pub trait HygieaUtcDateTimeExt: HygieaDateTimeExt {
    // ---- 日边界 ----------------------------------------------------------------

    /// 返回当前日期的开始。不会失败
    fn start_of_day(&self) -> Self;
    /// 返回当前日期的结束（23:59:59.999999999，time 的精度是 1 纳秒，这就是当天最后一个时刻）。不会失败。
    /// 做区间判断用半开区间 `[start_of_day, start_of_next_day)`
    fn end_of_day(&self) -> Self;
    /// 返回下一天的开始。
    fn start_of_next_day(&self) -> Result<Self, HyErr>;

    // ---- 周边界 ----------------------------------------------------------------

    /// 返回当前周的开始，周一为一周第一天。
    fn start_of_week(&self) -> Result<Self, HyErr>;
    /// 返回当前周的结束，周日为一周最后一天。做区间判断用半开区间 `[start_of_week, start_of_next_week)`
    fn end_of_week(&self) -> Result<Self, HyErr>;
    /// 返回下一周（下周一）的开始。
    fn start_of_next_week(&self) -> Result<Self, HyErr>;

    // ---- 月边界 ----------------------------------------------------------------

    /// 返回当前月的开始。
    fn start_of_month(&self) -> Result<Self, HyErr>;
    /// 返回当前月的结束。做区间判断用半开区间 `[start_of_month, start_of_next_month)`
    fn end_of_month(&self) -> Result<Self, HyErr>;
    /// 返回下个月的开始。
    fn start_of_next_month(&self) -> Result<Self, HyErr>;

    // ---- 年边界 ----------------------------------------------------------------

    /// 返回当前年的开始。
    fn start_of_year(&self) -> Result<Self, HyErr>;
    /// 返回当前年的结束。做区间判断用半开区间 `[start_of_year, start_of_next_year)`
    fn end_of_year(&self) -> Result<Self, HyErr>;
    /// 返回下一年的开始。
    fn start_of_next_year(&self) -> Result<Self, HyErr>;
}

impl HygieaDateTimeExt for UtcDateTime {
    fn from_millis(ms: i64) -> Result<Self, HyErr> {
        UtcDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("timestamp millis out of range: {ms}")
            )
        })
    }

    fn from_secs(s: i64) -> Result<Self, HyErr> {
        UtcDateTime::from_unix_timestamp(s).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("timestamp secs out of range: {s}")
            )
        })
    }

    fn parse_ext_rfc3339(s: &str) -> Result<Self, HyErr> {
        UtcDateTime::parse(s, &Rfc3339).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("parse RFC 3339 UTC datetime failed, input={s}")
            )
        })
    }

    fn shift(&self, duration: Duration) -> Result<Self, HyErr> {
        self.checked_add(duration).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("shift datetime out of range: datetime={self}, duration={duration}")
            )
        })
    }
}

impl HygieaUtcDateTimeExt for UtcDateTime {
    fn start_of_day(&self) -> Self {
        UtcDateTime::new(self.date(), Time::MIDNIGHT)
    }

    fn end_of_day(&self) -> Self {
        UtcDateTime::new(self.date(), Time::MAX)
    }

    fn start_of_next_day(&self) -> Result<Self, HyErr> {
        let date = self.date().next_day().ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("start_of_next_day out of range: datetime={self}")
            )
        })?;
        Ok(UtcDateTime::new(date, Time::MIDNIGHT))
    }

    fn start_of_week(&self) -> Result<Self, HyErr> {
        let midnight = self.start_of_day();
        let days = self.weekday().number_days_from_monday() as i64;
        midnight.checked_sub(Duration::days(days)).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("start_of_week out of range: datetime={self}")
            )
        })
    }

    fn end_of_week(&self) -> Result<Self, HyErr> {
        self.start_of_week()?
            .checked_add(Duration::days(7) - Duration::nanoseconds(1))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_week out of range: datetime={self}")
                )
            })
    }

    fn start_of_next_week(&self) -> Result<Self, HyErr> {
        self.start_of_week()?
            .checked_add(Duration::days(7))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("start_of_next_week out of range: datetime={self}")
                )
            })
    }

    fn start_of_month(&self) -> Result<Self, HyErr> {
        let date = self
            .date()
            .replace_day(1)
            .wrap_err(|| err!(BaseErr::DateError, "start_of_month failed"))?;
        Ok(UtcDateTime::new(date, Time::MIDNIGHT))
    }

    fn end_of_month(&self) -> Result<Self, HyErr> {
        let next_month = self
            .start_of_month()?
            .checked_add(Duration::days(31))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_month out of range: datetime={self}")
                )
            })?;
        let next_month = UtcDateTime::new(
            next_month
                .date()
                .replace_day(1)
                .wrap_err(|| err!(BaseErr::DateError, "end_of_month failed"))?,
            Time::MIDNIGHT,
        );
        next_month
            .checked_sub(Duration::nanoseconds(1))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_month out of range: datetime={self}")
                )
            })
    }

    fn start_of_next_month(&self) -> Result<Self, HyErr> {
        let next_month = self
            .start_of_month()?
            .checked_add(Duration::days(31))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("start_of_next_month out of range: datetime={self}")
                )
            })?;
        let date = next_month
            .date()
            .replace_day(1)
            .wrap_err(|| err!(BaseErr::DateError, "start_of_next_month failed"))?;
        Ok(UtcDateTime::new(date, Time::MIDNIGHT))
    }

    fn start_of_year(&self) -> Result<Self, HyErr> {
        let date = self
            .date()
            .replace_day(1)
            .and_then(|date| date.replace_month(Month::January))
            .wrap_err(|| err!(BaseErr::DateError, "start_of_year failed"))?;
        Ok(UtcDateTime::new(date, Time::MIDNIGHT))
    }

    fn end_of_year(&self) -> Result<Self, HyErr> {
        let next_year = self.year().checked_add(1).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("end_of_year out of range: datetime={self}")
            )
        })?;
        let date = self
            .start_of_year()?
            .date()
            .replace_year(next_year)
            .wrap_err(|| err!(BaseErr::DateError, "end_of_year failed"))?;
        UtcDateTime::new(date, Time::MIDNIGHT)
            .checked_sub(Duration::nanoseconds(1))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_year out of range: datetime={self}")
                )
            })
    }

    fn start_of_next_year(&self) -> Result<Self, HyErr> {
        let next_year = self.year().checked_add(1).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("start_of_next_year out of range: datetime={self}")
            )
        })?;
        let date = self
            .start_of_year()?
            .date()
            .replace_year(next_year)
            .wrap_err(|| err!(BaseErr::DateError, "start_of_next_year failed"))?;
        Ok(UtcDateTime::new(date, Time::MIDNIGHT))
    }
}

impl HygieaDateTimeExt for OffsetDateTime {
    fn from_millis(ms: i64) -> Result<OffsetDateTime, HyErr> {
        OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("timestamp millis out of range: {ms}")
            )
        })
    }

    fn from_secs(s: i64) -> Result<OffsetDateTime, HyErr> {
        OffsetDateTime::from_unix_timestamp(s).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("timestamp secs out of range: {s}")
            )
        })
    }

    fn parse_ext_rfc3339(s: &str) -> Result<OffsetDateTime, HyErr> {
        OffsetDateTime::parse(s, &Rfc3339).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("parse RFC 3339 datetime failed, input={s}")
            )
        })
    }

    fn shift(&self, duration: Duration) -> Result<OffsetDateTime, HyErr> {
        self.checked_add(duration).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("shift datetime out of range: datetime={self}, duration={duration}")
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use time::UtcOffset;
    use time::macros::{offset, utc_datetime};

    use super::*;
    use crate::datetime::*;

    // ---- fixtures ------------------------------------------------------------

    fn fixture_utc() -> UtcDateTime {
        utc_datetime!(2024-01-05 13:45:06.789_123_456)
    }

    /// 与 [`fixture_utc`] 同一时刻，固定 +08:00 offset。
    fn fixture_offset() -> OffsetDateTime {
        OffsetDateTime::from(fixture_utc()).to_offset(offset!(+8))
    }

    // ---- format --------------------------------------------------------------

    #[test]
    fn test_format_ext_utc_datetime() {
        let dt = fixture_utc();
        let without_offset = [
            (WithoutOffsetFormatter::Y, "2024"),
            (WithoutOffsetFormatter::HMS, "13:45:06"),
            (WithoutOffsetFormatter::Ymd, "2024-01-05"),
            (WithoutOffsetFormatter::Ymdnosep, "20240105"),
            (WithoutOffsetFormatter::YmdHMS, "2024-01-05 13:45:06"),
            (WithoutOffsetFormatter::YmdHMS3F, "2024-01-05 13:45:06.789"),
            (WithoutOffsetFormatter::YmdHMSnosep, "20240105134506"),
        ];
        for (formatter, expected) in without_offset {
            assert_eq!(
                HygieaDateTimeExt::format_ext(&dt, formatter).unwrap(),
                expected,
                "{formatter:?}"
            );
        }

        let with_offset = [
            (
                WithOffsetFormatter::YmdHMS3F,
                "2024-01-05 13:45:06.789 +00:00",
            ),
            (
                WithOffsetFormatter::YmdTHMS3F,
                "2024-01-05T13:45:06.789+00:00",
            ),
        ];
        for (formatter, expected) in with_offset {
            assert_eq!(
                HygieaDateTimeExt::format_ext(&dt, formatter).unwrap(),
                expected,
                "{formatter:?}"
            );
        }
    }

    #[test]
    fn test_format_ext_offset_datetime() {
        let dt = fixture_offset();
        let without_offset = [
            (WithoutOffsetFormatter::YmdHMS, "2024-01-05 21:45:06"),
            (WithoutOffsetFormatter::YmdHMSnosep, "20240105214506"),
        ];
        for (formatter, expected) in without_offset {
            assert_eq!(
                HygieaDateTimeExt::format_ext(&dt, formatter).unwrap(),
                expected,
                "{formatter:?}"
            );
        }

        let with_offset = [
            (
                WithOffsetFormatter::YmdHMS3F,
                "2024-01-05 21:45:06.789 +08:00",
            ),
            (
                WithOffsetFormatter::YmdTHMS3F,
                "2024-01-05T21:45:06.789+08:00",
            ),
        ];
        for (formatter, expected) in with_offset {
            assert_eq!(
                HygieaDateTimeExt::format_ext(&dt, formatter).unwrap(),
                expected,
                "{formatter:?}"
            );
        }

        // DateTimeFormatter::format 与 format_ext 等价
        assert_eq!(
            DateTimeFormatter::WithoutOffset(WithoutOffsetFormatter::Ymd)
                .format(&dt)
                .unwrap(),
            "2024-01-05"
        );
    }

    #[test]
    fn test_format_ext_rfc3339() {
        assert_eq!(
            HygieaDateTimeExt::format_ext_rfc3339(&fixture_utc()).unwrap(),
            "2024-01-05T13:45:06.789123456Z"
        );
        assert_eq!(
            HygieaDateTimeExt::format_ext_rfc3339(&fixture_offset()).unwrap(),
            "2024-01-05T21:45:06.789123456+08:00"
        );
    }

    // ---- parse ---------------------------------------------------------------

    #[test]
    fn test_parse_ext_rfc3339() {
        let dt = UtcDateTime::parse_ext_rfc3339("2024-01-05T13:45:06Z").unwrap();
        assert_eq!(dt, utc_datetime!(2024-01-05 13:45:06));
        // 带 offset 的输入换算到 UTC
        let dt = UtcDateTime::parse_ext_rfc3339("2024-01-05T21:45:06+08:00").unwrap();
        assert_eq!(dt, utc_datetime!(2024-01-05 13:45:06));
        // OffsetDateTime 保留输入 offset
        let dt = OffsetDateTime::parse_ext_rfc3339("2024-01-05T21:45:06+08:00").unwrap();
        assert_eq!(dt, OffsetDateTime::from(utc_datetime!(2024-01-05 13:45:06)));
        assert_eq!(dt.offset(), UtcOffset::from_hms(8, 0, 0).unwrap());
        // 非法输入
        assert!(UtcDateTime::parse_ext_rfc3339("not a datetime").is_err());
        assert!(OffsetDateTime::parse_ext_rfc3339("2024-13-05T13:45:06Z").is_err());
    }

    // ---- 时间戳 --------------------------------------------------------------

    #[test]
    fn test_from_unix_timestamps() {
        let dt = UtcDateTime::from_secs(1_704_067_200).unwrap();
        assert_eq!(dt, utc_datetime!(2024-01-01 0:00:00));
        let dt = UtcDateTime::from_millis(1_704_067_200_789).unwrap();
        assert_eq!(dt, utc_datetime!(2024-01-01 0:00:00.789_000_000));

        let dt = OffsetDateTime::from_secs(1_704_067_200).unwrap();
        assert_eq!(dt, OffsetDateTime::from(utc_datetime!(2024-01-01 0:00:00)));
        assert_eq!(
            OffsetDateTime::from_millis(1_704_067_200_789)
                .unwrap()
                .nanosecond(),
            789_000_000
        );

        // 超出 time 表示范围
        assert!(UtcDateTime::from_millis(i64::MAX).is_err());
        assert!(UtcDateTime::from_secs(i64::MAX).is_err());
        assert!(OffsetDateTime::from_millis(i64::MAX).is_err());
        assert!(OffsetDateTime::from_secs(i64::MAX).is_err());
    }

    // ---- shift ---------------------------------------------------------------

    #[test]
    fn test_shift() {
        let dt = fixture_utc();
        assert_eq!(
            dt.shift(Duration::hours(8)).unwrap(),
            utc_datetime!(2024-01-05 21:45:06.789_123_456)
        );
        assert_eq!(
            dt.shift(-Duration::days(5)).unwrap(),
            utc_datetime!(2023-12-31 13:45:06.789_123_456)
        );
        // 溢出
        assert!(UtcDateTime::MAX.shift(Duration::nanoseconds(1)).is_err());
        assert!(
            OffsetDateTime::from(UtcDateTime::MAX)
                .shift(Duration::nanoseconds(1))
                .is_err()
        );
        // OffsetDateTime 平移保留自身 offset
        let shifted = fixture_offset().shift(-Duration::hours(1)).unwrap();
        assert_eq!(shifted.offset(), UtcOffset::from_hms(8, 0, 0).unwrap());
        assert_eq!(
            shifted,
            OffsetDateTime::from(utc_datetime!(2024-01-05 12:45:06.789_123_456))
        );
    }

    // ---- UTC 日历边界 ----------------------------------------------------------

    #[test]
    fn test_utc_day_boundaries() {
        let dt = utc_datetime!(2024-01-03 15:30:45.000_000_123);
        assert_eq!(dt.start_of_day(), utc_datetime!(2024-01-03 0:00:00));
        assert_eq!(
            dt.end_of_day(),
            utc_datetime!(2024-01-03 23:59:59.999_999_999)
        );
    }

    #[test]
    fn test_utc_week_boundaries() {
        // 2024-01-01 是周一
        let wednesday = utc_datetime!(2024-01-03 15:30:45);
        assert_eq!(
            wednesday.start_of_week().unwrap(),
            utc_datetime!(2024-01-01 0:00:00)
        );
        assert_eq!(
            wednesday.end_of_week().unwrap(),
            utc_datetime!(2024-01-07 23:59:59.999_999_999)
        );

        // 周日仍在同一周
        let sunday = utc_datetime!(2024-01-07 23:00:00);
        assert_eq!(
            sunday.start_of_week().unwrap(),
            utc_datetime!(2024-01-01 0:00:00)
        );
        // 下周一开启新一周
        let monday = utc_datetime!(2024-01-08 0:30:00);
        assert_eq!(
            monday.start_of_week().unwrap(),
            utc_datetime!(2024-01-08 0:00:00)
        );
        assert_eq!(
            monday.end_of_week().unwrap(),
            utc_datetime!(2024-01-14 23:59:59.999_999_999)
        );
    }

    #[test]
    fn test_utc_month_boundaries() {
        let dt = utc_datetime!(2024-01-03 15:30:45);
        assert_eq!(
            dt.start_of_month().unwrap(),
            utc_datetime!(2024-01-01 0:00:00)
        );
        assert_eq!(
            dt.end_of_month().unwrap(),
            utc_datetime!(2024-01-31 23:59:59.999_999_999)
        );

        // 闰年 2 月
        let leap = utc_datetime!(2024-02-15 0:00:00);
        assert_eq!(
            leap.end_of_month().unwrap(),
            utc_datetime!(2024-02-29 23:59:59.999_999_999)
        );
        // 平年 2 月
        let common = utc_datetime!(2023-02-15 0:00:00);
        assert_eq!(
            common.end_of_month().unwrap(),
            utc_datetime!(2023-02-28 23:59:59.999_999_999)
        );
        // 30 天的月
        let april = utc_datetime!(2024-04-15 0:00:00);
        assert_eq!(
            april.end_of_month().unwrap(),
            utc_datetime!(2024-04-30 23:59:59.999_999_999)
        );
        // 12 月跨年
        let december = utc_datetime!(2024-12-15 0:00:00);
        assert_eq!(
            december.end_of_month().unwrap(),
            utc_datetime!(2024-12-31 23:59:59.999_999_999)
        );
    }

    #[test]
    fn test_utc_year_boundaries() {
        let dt = utc_datetime!(2024-07-01 12:00:00);
        assert_eq!(
            dt.start_of_year().unwrap(),
            utc_datetime!(2024-01-01 0:00:00)
        );
        assert_eq!(
            dt.end_of_year().unwrap(),
            utc_datetime!(2024-12-31 23:59:59.999_999_999)
        );
    }

    #[test]
    fn test_utc_start_of_next() {
        // 2024-01-03 是周三
        let dt = utc_datetime!(2024-01-03 15:30:45.000_000_123);
        assert_eq!(
            dt.start_of_next_day().unwrap(),
            utc_datetime!(2024-01-04 0:00:00)
        );
        assert_eq!(
            dt.start_of_next_week().unwrap(),
            utc_datetime!(2024-01-08 0:00:00)
        );
        assert_eq!(
            dt.start_of_next_month().unwrap(),
            utc_datetime!(2024-02-01 0:00:00)
        );
        assert_eq!(
            dt.start_of_next_year().unwrap(),
            utc_datetime!(2025-01-01 0:00:00)
        );

        // 跨月、跨年：12 月 31 日（周二）
        let dec = utc_datetime!(2024-12-31 12:00:00);
        assert_eq!(
            dec.start_of_next_day().unwrap(),
            utc_datetime!(2025-01-01 0:00:00)
        );
        assert_eq!(
            dec.start_of_next_week().unwrap(),
            utc_datetime!(2025-01-06 0:00:00)
        );
        assert_eq!(
            dec.start_of_next_month().unwrap(),
            utc_datetime!(2025-01-01 0:00:00)
        );
        // 周日的下一周就是第二天
        let sunday = utc_datetime!(2024-01-07 12:00:00);
        assert_eq!(
            sunday.start_of_next_week().unwrap(),
            utc_datetime!(2024-01-08 0:00:00)
        );
        // 闰年 2 月
        let leap = utc_datetime!(2024-02-29 12:00:00);
        assert_eq!(
            leap.start_of_next_month().unwrap(),
            utc_datetime!(2024-03-01 0:00:00)
        );
        // 和 end_of_* 正好相差 1 纳秒
        assert_eq!(
            dt.start_of_next_month().unwrap() - dt.end_of_month().unwrap(),
            Duration::nanoseconds(1)
        );

        // 越界
        let max = UtcDateTime::MAX;
        assert!(max.start_of_next_day().unwrap_err().is(BaseErr::DateError));
        assert!(max.start_of_next_week().unwrap_err().is(BaseErr::DateError));
        assert!(
            max.start_of_next_month()
                .unwrap_err()
                .is(BaseErr::DateError)
        );
        assert!(max.start_of_next_year().unwrap_err().is(BaseErr::DateError));
    }
}
