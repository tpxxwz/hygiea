use crate::{BaseErr, HyErr, ResultExt, err};
#[cfg(any(test, feature = "datetime-sim-clock"))]
use parking_lot::RwLock;
use std::str::FromStr;
use time::format_description::{BorrowedFormatItem, well_known::Rfc3339};
use time::formatting::Formattable;
use time::macros::format_description;

use time::{Duration, Month, OffsetDateTime, Time, UtcDateTime};

#[cfg(any(test, feature = "datetime-sim-clock"))]
mod clock;
#[cfg(any(test, feature = "datetime-sim-clock"))]
pub use clock::SimClock;

#[cfg(any(test, feature = "datetime-sim-clock"))]
static NOW_FN: RwLock<fn() -> UtcDateTime> = RwLock::new(UtcDateTime::now);

#[doc(hidden)]
#[cfg(any(test, feature = "datetime-sim-clock"))]
pub fn set_now_utc(f: fn() -> UtcDateTime) {
    *NOW_FN.write() = f;
}

#[cfg(any(test, feature = "datetime-sim-clock"))]
pub fn now_utc() -> UtcDateTime {
    let now = *NOW_FN.read();
    now()
}

#[cfg(not(any(test, feature = "datetime-sim-clock")))]
pub fn now_utc() -> UtcDateTime {
    UtcDateTime::now()
}

/// 当前 UTC 时刻的 `OffsetDateTime` 表示。
pub fn now() -> OffsetDateTime {
    now_utc().into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateTimeFormatter {
    WithOffset(WithOffsetFormatter),
    WithoutOffset(WithoutOffsetFormatter),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithOffsetFormatter {
    YmdHMS3F,
    YmdTHMS3F,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithoutOffsetFormatter {
    Y,
    HMS,
    Ymd,
    Ymdnosep,
    YmdHMS,
    YmdHMS3F,
    YmdHMSnosep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithOffsetParser {
    YmdHMS,
    YmdHMS3F,
    YmdHMSnosep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithoutOffsetParser {
    YmdHMS,
    YmdHMS3F,
    YmdHMSnosep,
}

impl From<WithOffsetFormatter> for DateTimeFormatter {
    fn from(formatter: WithOffsetFormatter) -> Self {
        Self::WithOffset(formatter)
    }
}

impl From<WithoutOffsetFormatter> for DateTimeFormatter {
    fn from(formatter: WithoutOffsetFormatter) -> Self {
        Self::WithoutOffset(formatter)
    }
}

impl FromStr for DateTimeFormatter {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "WithOffset.YmdHMS3F" => Ok(Self::WithOffset(WithOffsetFormatter::YmdHMS3F)),
            "WithOffset.YmdTHMS3F" => Ok(Self::WithOffset(WithOffsetFormatter::YmdTHMS3F)),
            "WithoutOffset.Y" => Ok(Self::WithoutOffset(WithoutOffsetFormatter::Y)),
            "WithoutOffset.HMS" => Ok(Self::WithoutOffset(WithoutOffsetFormatter::HMS)),
            "WithoutOffset.Ymd" => Ok(Self::WithoutOffset(WithoutOffsetFormatter::Ymd)),
            "WithoutOffset.Ymdnosep" => Ok(Self::WithoutOffset(WithoutOffsetFormatter::Ymdnosep)),
            "WithoutOffset.YmdHMS" => Ok(Self::WithoutOffset(WithoutOffsetFormatter::YmdHMS)),
            "WithoutOffset.YmdHMS3F" => Ok(Self::WithoutOffset(WithoutOffsetFormatter::YmdHMS3F)),
            "WithoutOffset.YmdHMSnosep" => {
                Ok(Self::WithoutOffset(WithoutOffsetFormatter::YmdHMSnosep))
            }
            _ => Err(format!("unsupported date time formatter: {value}")),
        }
    }
}

impl Default for DateTimeFormatter {
    fn default() -> Self {
        Self::WithOffset(WithOffsetFormatter::YmdTHMS3F)
    }
}

// 给 TracingConfig.time_format 从配置文件反序列化用，serde 也是 log feature 带进来的
#[cfg(feature = "log")]
impl<'de> serde::Deserialize<'de> for DateTimeFormatter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl From<WithoutOffsetParser> for WithoutOffsetFormatter {
    fn from(parser: WithoutOffsetParser) -> Self {
        match parser {
            WithoutOffsetParser::YmdHMS => Self::YmdHMS,
            WithoutOffsetParser::YmdHMS3F => Self::YmdHMS3F,
            WithoutOffsetParser::YmdHMSnosep => Self::YmdHMSnosep,
        }
    }
}

impl WithoutOffsetFormatter {
    fn description(self) -> &'static [BorrowedFormatItem<'static>] {
        match self {
            Self::Y => format_description!("[year]"),
            Self::HMS => format_description!("[hour]:[minute]:[second]"),
            Self::Ymd => format_description!("[year]-[month]-[day]"),
            Self::Ymdnosep => format_description!("[year][month][day]"),
            Self::YmdHMS => format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
            Self::YmdHMS3F => format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"
            ),
            Self::YmdHMSnosep => format_description!("[year][month][day][hour][minute][second]"),
        }
    }
}

impl WithOffsetFormatter {
    fn description(self) -> &'static [BorrowedFormatItem<'static>] {
        match self {
            Self::YmdHMS3F => format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3] [offset_hour sign:mandatory]:[offset_minute]"
            ),
            Self::YmdTHMS3F => format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3][offset_hour sign:mandatory]:[offset_minute]"
            ),
        }
    }
}

impl WithOffsetParser {
    fn description(self) -> &'static [BorrowedFormatItem<'static>] {
        match self {
            Self::YmdHMS => format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory]:[offset_minute]"
            ),
            Self::YmdHMS3F => format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3] [offset_hour sign:mandatory]:[offset_minute]"
            ),
            Self::YmdHMSnosep => format_description!(
                "[year][month][day][hour][minute][second][offset_hour sign:mandatory][offset_minute]"
            ),
        }
    }

    pub fn parse(self, input: &str) -> Result<OffsetDateTime, HyErr> {
        OffsetDateTime::parse(input, self.description()).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("parse datetime with offset failed, input={input}")
            )
        })
    }
}

#[doc(hidden)]
pub trait DateTimeFormattable {
    fn format_with(
        &self,
        formatter: &(impl Formattable + ?Sized),
    ) -> Result<String, time::error::Format>;

    fn format_ext_rfc3339(&self) -> Result<String, HyErr> {
        self.format_with(&Rfc3339)
            .wrap_err(|| err!(BaseErr::DateError, "format RFC 3339 datetime failed"))
    }

    fn format_ext(&self, formatter: DateTimeFormatter) -> Result<String, HyErr> {
        let result = match formatter {
            DateTimeFormatter::WithOffset(formatter) => self.format_with(formatter.description()),
            DateTimeFormatter::WithoutOffset(formatter) => {
                self.format_with(formatter.description())
            }
        };
        result.wrap_err(|| err!(BaseErr::DateError, "format datetime failed"))
    }
}

impl DateTimeFormattable for UtcDateTime {
    fn format_with(
        &self,
        formatter: &(impl Formattable + ?Sized),
    ) -> Result<String, time::error::Format> {
        self.format(formatter)
    }
}

impl DateTimeFormattable for OffsetDateTime {
    fn format_with(
        &self,
        formatter: &(impl Formattable + ?Sized),
    ) -> Result<String, time::error::Format> {
        self.format(formatter)
    }
}

impl DateTimeFormatter {
    pub fn format<T: HygieaDateTimeExt>(self, datetime: &T) -> Result<String, HyErr> {
        HygieaDateTimeExt::format_ext(datetime, self)
    }
}

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
        DateTimeFormattable::format_ext_rfc3339(self)
    }
    /// 按指定格式输出日期时间。
    fn format_ext(&self, formatter: impl Into<DateTimeFormatter>) -> Result<String, HyErr> {
        DateTimeFormattable::format_ext(self, formatter.into())
    }

    /// 在绝对时间线上平移指定时长。
    fn shift(&self, duration: Duration) -> Result<Self, HyErr>;
}

/// `UtcDateTime` 专有的 UTC 日历边界扩展。
pub trait HygieaUtcDateTimeExt: HygieaDateTimeExt {
    // ---- 日边界 ----------------------------------------------------------------

    /// 返回当前日期的开始。
    fn start_of_day(&self) -> Result<Self, HyErr>;
    /// 返回当前日期的结束。
    fn end_of_day(&self) -> Result<Self, HyErr>;

    // ---- 周边界 ----------------------------------------------------------------

    /// 返回当前周的开始，周一为一周第一天。
    fn start_of_week(&self) -> Result<Self, HyErr>;
    /// 返回当前周的结束，周日为一周最后一天。
    fn end_of_week(&self) -> Result<Self, HyErr>;

    // ---- 月边界 ----------------------------------------------------------------

    /// 返回当前月的开始。
    fn start_of_month(&self) -> Result<Self, HyErr>;
    /// 返回当前月的结束。
    fn end_of_month(&self) -> Result<Self, HyErr>;

    // ---- 年边界 ----------------------------------------------------------------

    /// 返回当前年的开始。
    fn start_of_year(&self) -> Result<Self, HyErr>;
    /// 返回当前年的结束。
    fn end_of_year(&self) -> Result<Self, HyErr>;
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
    fn start_of_day(&self) -> Result<Self, HyErr> {
        Ok(UtcDateTime::new(self.date(), Time::MIDNIGHT))
    }

    fn end_of_day(&self) -> Result<Self, HyErr> {
        Ok(UtcDateTime::new(self.date(), Time::MAX))
    }

    fn start_of_week(&self) -> Result<Self, HyErr> {
        let midnight = self.start_of_day()?;
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
    use super::*;
    use crate::datetime::SimClock;
    use serial_test::serial;
    use time::{Date, UtcOffset};

    // ---- fixtures ------------------------------------------------------------

    fn utc_nano(y: i32, m: Month, d: u8, h: u8, mi: u8, s: u8, nanos: u32) -> UtcDateTime {
        Date::from_calendar_date(y, m, d)
            .unwrap()
            .with_time(Time::from_hms_nano(h, mi, s, nanos).unwrap())
            .as_utc()
    }

    fn utc(y: i32, m: Month, d: u8, h: u8, mi: u8, s: u8) -> UtcDateTime {
        utc_nano(y, m, d, h, mi, s, 0)
    }

    fn day_end(y: i32, m: Month, d: u8) -> UtcDateTime {
        utc_nano(y, m, d, 23, 59, 59, 999_999_999)
    }

    fn fixture_utc() -> UtcDateTime {
        utc_nano(2024, Month::January, 5, 13, 45, 6, 789_123_456)
    }

    /// 与 [`fixture_utc`] 同一时刻，固定 +08:00 offset。
    fn fixture_offset() -> OffsetDateTime {
        let offset = UtcOffset::from_hms(8, 0, 0).unwrap();
        OffsetDateTime::from(fixture_utc()).to_offset(offset)
    }

    // ---- clock ---------------------------------------------------------------

    #[test]
    #[serial]
    fn test_now_utc_override_and_restore() {
        let fixed = utc(2024, Month::January, 5, 13, 45, 6);
        {
            let _clock = SimClock::new(fixed);
            assert_eq!(now_utc(), fixed);
            assert_eq!(now(), OffsetDateTime::from(fixed));
        }
        // SimClock Drop 后恢复真实时钟
        let real = UtcDateTime::now();
        assert!((now_utc() - real).whole_seconds().abs() < 60);
    }

    // ---- FromStr / From ------------------------------------------------------

    #[test]
    fn test_datetime_formatter_from_str() {
        use WithOffsetFormatter as O;
        use WithoutOffsetFormatter as W;

        let cases = [
            (
                "WithOffset.YmdHMS3F",
                DateTimeFormatter::WithOffset(O::YmdHMS3F),
            ),
            (
                "WithOffset.YmdTHMS3F",
                DateTimeFormatter::WithOffset(O::YmdTHMS3F),
            ),
            ("WithoutOffset.Y", DateTimeFormatter::WithoutOffset(W::Y)),
            (
                "WithoutOffset.HMS",
                DateTimeFormatter::WithoutOffset(W::HMS),
            ),
            (
                "WithoutOffset.Ymd",
                DateTimeFormatter::WithoutOffset(W::Ymd),
            ),
            (
                "WithoutOffset.Ymdnosep",
                DateTimeFormatter::WithoutOffset(W::Ymdnosep),
            ),
            (
                "WithoutOffset.YmdHMS",
                DateTimeFormatter::WithoutOffset(W::YmdHMS),
            ),
            (
                "WithoutOffset.YmdHMS3F",
                DateTimeFormatter::WithoutOffset(W::YmdHMS3F),
            ),
            (
                "WithoutOffset.YmdHMSnosep",
                DateTimeFormatter::WithoutOffset(W::YmdHMSnosep),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(input.parse::<DateTimeFormatter>().unwrap(), expected);
        }
        let error = "nonsense".parse::<DateTimeFormatter>().unwrap_err();
        assert!(error.contains("unsupported"));
    }

    #[test]
    fn test_formatter_conversions() {
        assert_eq!(
            DateTimeFormatter::from(WithOffsetFormatter::YmdTHMS3F),
            DateTimeFormatter::default()
        );
        assert_eq!(
            DateTimeFormatter::from(WithoutOffsetFormatter::YmdHMS),
            DateTimeFormatter::WithoutOffset(WithoutOffsetFormatter::YmdHMS)
        );
        assert_eq!(
            WithoutOffsetFormatter::from(WithoutOffsetParser::YmdHMS3F),
            WithoutOffsetFormatter::YmdHMS3F
        );
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
        assert_eq!(dt, utc(2024, Month::January, 5, 13, 45, 6));
        // 带 offset 的输入换算到 UTC
        let dt = UtcDateTime::parse_ext_rfc3339("2024-01-05T21:45:06+08:00").unwrap();
        assert_eq!(dt, utc(2024, Month::January, 5, 13, 45, 6));
        // OffsetDateTime 保留输入 offset
        let dt = OffsetDateTime::parse_ext_rfc3339("2024-01-05T21:45:06+08:00").unwrap();
        assert_eq!(
            dt,
            OffsetDateTime::from(utc(2024, Month::January, 5, 13, 45, 6))
        );
        assert_eq!(dt.offset(), UtcOffset::from_hms(8, 0, 0).unwrap());
        // 非法输入
        assert!(UtcDateTime::parse_ext_rfc3339("not a datetime").is_err());
        assert!(OffsetDateTime::parse_ext_rfc3339("2024-13-05T13:45:06Z").is_err());
    }

    #[test]
    fn test_with_offset_parser_parse() {
        let dt = WithOffsetParser::YmdHMS
            .parse("2024-01-05 21:45:06 +08:00")
            .unwrap();
        assert_eq!(dt.hour(), 21);
        assert_eq!(dt.offset(), UtcOffset::from_hms(8, 0, 0).unwrap());

        let dt = WithOffsetParser::YmdHMS3F
            .parse("2024-01-05 21:45:06.789 +08:00")
            .unwrap();
        assert_eq!(dt.nanosecond(), 789_000_000);

        let nosep = WithOffsetParser::YmdHMSnosep
            .parse("20240105214506+0800")
            .unwrap();
        let separated = WithOffsetParser::YmdHMS
            .parse("2024-01-05 21:45:06 +08:00")
            .unwrap();
        assert_eq!(nosep, separated);

        // 缺 offset 报错
        assert!(
            WithOffsetParser::YmdHMS
                .parse("2024-01-05 21:45:06")
                .is_err()
        );
    }

    // ---- 时间戳 --------------------------------------------------------------

    #[test]
    fn test_from_unix_timestamps() {
        let dt = UtcDateTime::from_secs(1_704_067_200).unwrap();
        assert_eq!(dt, utc(2024, Month::January, 1, 0, 0, 0));
        let dt = UtcDateTime::from_millis(1_704_067_200_789).unwrap();
        assert_eq!(dt, utc_nano(2024, Month::January, 1, 0, 0, 0, 789_000_000));

        let dt = OffsetDateTime::from_secs(1_704_067_200).unwrap();
        assert_eq!(
            dt,
            OffsetDateTime::from(utc(2024, Month::January, 1, 0, 0, 0))
        );
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
            utc_nano(2024, Month::January, 5, 21, 45, 6, 789_123_456)
        );
        assert_eq!(
            dt.shift(-Duration::days(5)).unwrap(),
            utc_nano(2023, Month::December, 31, 13, 45, 6, 789_123_456)
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
            OffsetDateTime::from(utc_nano(2024, Month::January, 5, 12, 45, 6, 789_123_456))
        );
    }

    // ---- UTC 日历边界 ----------------------------------------------------------

    #[test]
    fn test_utc_day_boundaries() {
        let dt = utc_nano(2024, Month::January, 3, 15, 30, 45, 123);
        assert_eq!(
            dt.start_of_day().unwrap(),
            utc(2024, Month::January, 3, 0, 0, 0)
        );
        assert_eq!(dt.end_of_day().unwrap(), day_end(2024, Month::January, 3));
    }

    #[test]
    fn test_utc_week_boundaries() {
        // 2024-01-01 是周一
        let wednesday = utc(2024, Month::January, 3, 15, 30, 45);
        assert_eq!(
            wednesday.start_of_week().unwrap(),
            utc(2024, Month::January, 1, 0, 0, 0)
        );
        assert_eq!(
            wednesday.end_of_week().unwrap(),
            day_end(2024, Month::January, 7)
        );

        // 周日仍在同一周
        let sunday = utc(2024, Month::January, 7, 23, 0, 0);
        assert_eq!(
            sunday.start_of_week().unwrap(),
            utc(2024, Month::January, 1, 0, 0, 0)
        );
        // 下周一开启新一周
        let monday = utc(2024, Month::January, 8, 0, 30, 0);
        assert_eq!(
            monday.start_of_week().unwrap(),
            utc(2024, Month::January, 8, 0, 0, 0)
        );
        assert_eq!(
            monday.end_of_week().unwrap(),
            day_end(2024, Month::January, 14)
        );
    }

    #[test]
    fn test_utc_month_boundaries() {
        let dt = utc(2024, Month::January, 3, 15, 30, 45);
        assert_eq!(
            dt.start_of_month().unwrap(),
            utc(2024, Month::January, 1, 0, 0, 0)
        );
        assert_eq!(
            dt.end_of_month().unwrap(),
            day_end(2024, Month::January, 31)
        );

        // 闰年 2 月
        let leap = utc(2024, Month::February, 15, 0, 0, 0);
        assert_eq!(
            leap.end_of_month().unwrap(),
            day_end(2024, Month::February, 29)
        );
        // 平年 2 月
        let common = utc(2023, Month::February, 15, 0, 0, 0);
        assert_eq!(
            common.end_of_month().unwrap(),
            day_end(2023, Month::February, 28)
        );
        // 30 天的月
        let april = utc(2024, Month::April, 15, 0, 0, 0);
        assert_eq!(
            april.end_of_month().unwrap(),
            day_end(2024, Month::April, 30)
        );
        // 12 月跨年
        let december = utc(2024, Month::December, 15, 0, 0, 0);
        assert_eq!(
            december.end_of_month().unwrap(),
            day_end(2024, Month::December, 31)
        );
    }

    #[test]
    fn test_utc_year_boundaries() {
        let dt = utc(2024, Month::July, 1, 12, 0, 0);
        assert_eq!(
            dt.start_of_year().unwrap(),
            utc(2024, Month::January, 1, 0, 0, 0)
        );
        assert_eq!(
            dt.end_of_year().unwrap(),
            day_end(2024, Month::December, 31)
        );
    }
}

#[cfg(feature = "datetime-iana")]
pub(super) mod iana {
    use super::*;
    use std::sync::OnceLock;
    use time::PrimitiveDateTime;
    use time_tz::{OffsetDateTimeExt, OffsetResult, PrimitiveDateTimeExt, Tz};

    static LOCAL_TIMEZONE: OnceLock<&'static Tz> = OnceLock::new();

    #[cfg(test)]
    static LOCAL_TIMEZONE_OVERRIDE: RwLock<Option<&'static Tz>> = RwLock::new(None);

    /// 测试专用：覆盖 `local_timezone()` 返回的时区，`None` 恢复系统时区。
    #[cfg(test)]
    pub(super) fn set_local_timezone(tz: Option<&'static Tz>) {
        *LOCAL_TIMEZONE_OVERRIDE.write() = tz;
    }

    pub(crate) fn init() {
        let _ = local_timezone();
    }

    fn local_timezone() -> &'static Tz {
        #[cfg(test)]
        if let Some(tz) = *LOCAL_TIMEZONE_OVERRIDE.read() {
            return tz;
        }
        LOCAL_TIMEZONE.get_or_init(|| {
            time_tz::system::get_timezone().unwrap_or_else(|error| {
                eprintln!(
                    "ERROR hygiea: failed to determine system local timezone: {error}; falling back to UTC"
                );
                time_tz::timezones::db::UTC
            })
        })
    }

    fn resolve_local(datetime: PrimitiveDateTime) -> OffsetResult<OffsetDateTime> {
        datetime.assume_timezone(local_timezone())
    }

    /// 当前时刻转换到系统 IANA 时区在该时刻使用的本地 offset。
    pub fn now_local() -> OffsetDateTime {
        now().to_timezone(local_timezone())
    }

    impl WithoutOffsetParser {
        fn description(self) -> &'static [BorrowedFormatItem<'static>] {
            WithoutOffsetFormatter::from(self).description()
        }

        pub fn parse(self, input: &str) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            let datetime = PrimitiveDateTime::parse(input, self.description()).wrap_err(|| {
                err!(
                    BaseErr::DateError,
                    format!("parse datetime without offset failed, input={input}")
                )
            })?;
            Ok(resolve_local(datetime))
        }
    }

    /// `OffsetDateTime` 专有的系统 IANA 本地时区扩展。
    pub trait HygieaOffsetDateTimeExt: HygieaDateTimeExt {
        // ---- 解析与格式化 ------------------------------------------------------

        /// 按携带 offset 的指定格式解析，并保留输入中的 offset。
        fn parse_ext_with_offset(
            s: &str,
            parser: WithOffsetParser,
        ) -> Result<OffsetDateTime, HyErr>;
        /// 将不携带 offset 的钟面时间按缓存的系统 IANA 时区解析。
        ///
        /// 返回值保留唯一、歧义或不存在三种时区解析结果。
        fn parse_ext_local(
            s: &str,
            parser: WithoutOffsetParser,
        ) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
        /// 转换到系统 IANA 时区在该时间点使用的 offset，再按指定格式输出。
        fn format_ext_local(&self, formatter: DateTimeFormatter) -> Result<String, HyErr>;

        // ---- 时间平移 ------------------------------------------------------------

        /// 在绝对时间线上平移指定时长，再转换到系统 IANA 时区。
        fn shift_local(&self, duration: Duration) -> Result<OffsetDateTime, HyErr>;

        // ---- 日边界 ----------------------------------------------------------------

        /// 按系统 IANA 时区返回当前本地日期的开始。
        fn start_of_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
        /// 按系统 IANA 时区返回当前本地日期的结束。
        fn end_of_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;

        // ---- 周边界 ----------------------------------------------------------------

        /// 按系统 IANA 时区返回当前本地周的开始，周一为一周第一天。
        fn start_of_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
        /// 按系统 IANA 时区返回当前本地周的结束，周日为一周最后一天。
        fn end_of_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;

        // ---- 月边界 ----------------------------------------------------------------

        /// 按系统 IANA 时区返回当前本地月的开始。
        fn start_of_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
        /// 按系统 IANA 时区返回当前本地月的结束。
        fn end_of_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;

        // ---- 年边界 ----------------------------------------------------------------

        /// 按系统 IANA 时区返回当前本地年的开始。
        fn start_of_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
        /// 按系统 IANA 时区返回当前本地年的结束。
        fn end_of_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    }

    impl HygieaOffsetDateTimeExt for OffsetDateTime {
        fn parse_ext_with_offset(
            s: &str,
            parser: WithOffsetParser,
        ) -> Result<OffsetDateTime, HyErr> {
            parser.parse(s)
        }

        fn parse_ext_local(
            s: &str,
            parser: WithoutOffsetParser,
        ) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            parser.parse(s)
        }

        fn format_ext_local(&self, formatter: DateTimeFormatter) -> Result<String, HyErr> {
            HygieaDateTimeExt::format_ext(&self.to_timezone(local_timezone()), formatter)
        }

        fn shift_local(&self, duration: Duration) -> Result<OffsetDateTime, HyErr> {
            self.checked_add(duration)
                .map(|datetime| datetime.to_timezone(local_timezone()))
                .ok_or_else(|| {
                    err!(
                        BaseErr::DateError,
                        format!(
                            "shift datetime out of range: datetime={self}, duration={duration}"
                        )
                    )
                })
        }

        fn start_of_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            let local = self.to_timezone(local_timezone());
            Ok(resolve_local(PrimitiveDateTime::new(
                local.date(),
                Time::MIDNIGHT,
            )))
        }

        fn end_of_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            let local = self.to_timezone(local_timezone());
            Ok(resolve_local(PrimitiveDateTime::new(
                local.date(),
                Time::MAX,
            )))
        }

        fn start_of_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            let local = self.to_timezone(local_timezone());
            let days = local.weekday().number_days_from_monday() as i64;
            let date = local
                .date()
                .checked_sub(Duration::days(days))
                .ok_or_else(|| {
                    err!(
                        BaseErr::DateError,
                        format!("start_of_week_local out of range: datetime={self}")
                    )
                })?;
            Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
        }

        fn end_of_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            let local = self.to_timezone(local_timezone());
            let days = 6 - local.weekday().number_days_from_monday() as i64;
            let date = local
                .date()
                .checked_add(Duration::days(days))
                .ok_or_else(|| {
                    err!(
                        BaseErr::DateError,
                        format!("end_of_week_local out of range: datetime={self}")
                    )
                })?;
            Ok(resolve_local(PrimitiveDateTime::new(date, Time::MAX)))
        }

        fn start_of_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            let local = self.to_timezone(local_timezone());
            let date = local
                .date()
                .replace_day(1)
                .wrap_err(|| err!(BaseErr::DateError, "start_of_month_local failed"))?;
            Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
        }

        fn end_of_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            let local = self.to_timezone(local_timezone());
            let first = PrimitiveDateTime::new(
                local
                    .date()
                    .replace_day(1)
                    .wrap_err(|| err!(BaseErr::DateError, "end_of_month_local failed"))?,
                Time::MIDNIGHT,
            );
            let next = first
                .checked_add(Duration::days(31))
                .and_then(|datetime| datetime.replace_day(1).ok())
                .ok_or_else(|| {
                    err!(
                        BaseErr::DateError,
                        format!("end_of_month_local out of range: datetime={self}")
                    )
                })?;
            let end = next.checked_sub(Duration::nanoseconds(1)).ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_month_local out of range: datetime={self}")
                )
            })?;
            Ok(resolve_local(end))
        }

        fn start_of_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            let local = self.to_timezone(local_timezone());
            let date = local
                .date()
                .replace_day(1)
                .and_then(|date| date.replace_month(Month::January))
                .wrap_err(|| err!(BaseErr::DateError, "start_of_year_local failed"))?;
            Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
        }

        fn end_of_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
            let local = self.to_timezone(local_timezone());
            let next_year = local.year().checked_add(1).ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_year_local out of range: datetime={self}")
                )
            })?;
            let date = local
                .date()
                .replace_day(1)
                .and_then(|date| date.replace_month(Month::January))
                .and_then(|date| date.replace_year(next_year))
                .wrap_err(|| err!(BaseErr::DateError, "end_of_year_local failed"))?;
            let end = PrimitiveDateTime::new(date, Time::MIDNIGHT)
                .checked_sub(Duration::nanoseconds(1))
                .ok_or_else(|| {
                    err!(
                        BaseErr::DateError,
                        format!("end_of_year_local out of range: datetime={self}")
                    )
                })?;
            Ok(resolve_local(end))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::datetime::SimClock;
        use serial_test::serial;
        use time::{Date, UtcOffset};

        // ---- fixtures --------------------------------------------------------

        fn tz(name: &str) -> &'static Tz {
            time_tz::timezones::get_by_name(name).expect("known timezone")
        }

        fn off(hours: i8) -> UtcOffset {
            UtcOffset::from_hms(hours, 0, 0).unwrap()
        }

        fn local(
            y: i32,
            m: Month,
            d: u8,
            h: u8,
            mi: u8,
            s: u8,
            nanos: u32,
            offset: UtcOffset,
        ) -> OffsetDateTime {
            Date::from_calendar_date(y, m, d)
                .unwrap()
                .with_time(Time::from_hms_nano(h, mi, s, nanos).unwrap())
                .assume_offset(offset)
        }

        fn unique(result: Result<OffsetResult<OffsetDateTime>, HyErr>) -> OffsetDateTime {
            match result {
                Ok(OffsetResult::Some(dt)) => dt,
                Ok(other) => panic!("expected unique local datetime, got {other:?}"),
                Err(e) => panic!("expected Ok, got {e:?}"),
            }
        }

        fn assert_local_eq(actual: OffsetDateTime, expected: OffsetDateTime) {
            assert_eq!(actual, expected, "absolute datetime differs");
            assert_eq!(actual.offset(), expected.offset(), "UTC offset differs");
            assert_eq!(actual.date(), expected.date(), "local date differs");
            assert_eq!(actual.time(), expected.time(), "local time differs");
        }

        struct TzGuard;

        impl TzGuard {
            fn new(tz: &'static Tz) -> Self {
                set_local_timezone(Some(tz));
                Self
            }
        }

        impl Drop for TzGuard {
            fn drop(&mut self) {
                set_local_timezone(None);
            }
        }

        // ---- now_local -------------------------------------------------------

        #[test]
        #[serial]
        fn test_now_local() {
            let tz = tz("Asia/Shanghai");
            let _tz_guard = TzGuard::new(tz);
            let fixed = local(2024, Month::January, 5, 13, 45, 6, 0, off(0)).to_utc();
            let _clock = SimClock::new(fixed);
            assert_eq!(now_local(), OffsetDateTime::from(fixed).to_timezone(tz));
            assert_eq!(now_local().offset(), off(8));
        }

        // ---- parse -----------------------------------------------------------

        #[test]
        fn test_parse_ext_with_offset() {
            let dt = OffsetDateTime::parse_ext_with_offset(
                "2024-01-05 21:45:06 +08:00",
                WithOffsetParser::YmdHMS,
            )
            .unwrap();
            assert_eq!(dt.hour(), 21);
            assert_eq!(dt.offset(), off(8));

            let dt = OffsetDateTime::parse_ext_with_offset(
                "2024-01-05 21:45:06.789 +08:00",
                WithOffsetParser::YmdHMS3F,
            )
            .unwrap();
            assert_eq!(dt.nanosecond(), 789_000_000);

            let nosep = OffsetDateTime::parse_ext_with_offset(
                "20240105214506+0800",
                WithOffsetParser::YmdHMSnosep,
            )
            .unwrap();
            let separated = OffsetDateTime::parse_ext_with_offset(
                "2024-01-05 21:45:06 +08:00",
                WithOffsetParser::YmdHMS,
            )
            .unwrap();
            assert_eq!(nosep, separated);

            // 缺 offset 报错
            assert!(
                OffsetDateTime::parse_ext_with_offset(
                    "2024-01-05 21:45:06",
                    WithOffsetParser::YmdHMS
                )
                .is_err()
            );
        }

        #[test]
        #[serial]
        fn test_parse_ext_local_unique() {
            let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
            let OffsetResult::Some(dt) =
                OffsetDateTime::parse_ext_local("2024-01-05 13:45:06", WithoutOffsetParser::YmdHMS)
                    .unwrap()
            else {
                panic!("expected unique local datetime");
            };
            assert_eq!(dt, local(2024, Month::January, 5, 5, 45, 6, 0, off(0)));
            assert_eq!(dt.offset(), off(8));

            let OffsetResult::Some(dt) = OffsetDateTime::parse_ext_local(
                "2024-01-05 13:45:06.789",
                WithoutOffsetParser::YmdHMS3F,
            )
            .unwrap() else {
                panic!("expected unique local datetime");
            };
            assert_eq!(
                dt,
                local(2024, Month::January, 5, 5, 45, 6, 789_000_000, off(0))
            );

            let OffsetResult::Some(dt) =
                OffsetDateTime::parse_ext_local("20240105134506", WithoutOffsetParser::YmdHMSnosep)
                    .unwrap()
            else {
                panic!("expected unique local datetime");
            };
            assert_eq!(dt, local(2024, Month::January, 5, 5, 45, 6, 0, off(0)));

            // 格式非法报错
            assert!(
                OffsetDateTime::parse_ext_local("2024/01/05 13:45:06", WithoutOffsetParser::YmdHMS)
                    .is_err()
            );
        }

        #[test]
        #[serial]
        fn test_parse_ext_local_dst_gap_and_overlap() {
            let _tz_guard = TzGuard::new(tz("America/New_York"));
            // 2024-03-10 02:00 EST 春季拨快到 03:00 EDT，02:30 的钟面时间不存在
            assert!(matches!(
                OffsetDateTime::parse_ext_local("2024-03-10 02:30:45", WithoutOffsetParser::YmdHMS)
                    .unwrap(),
                OffsetResult::None
            ));
            // 2024-11-03 02:00 EDT 秋季拨回 01:00 EST，01:30 的钟面时间对应两个时刻
            let OffsetResult::Ambiguous(first, second) =
                OffsetDateTime::parse_ext_local("2024-11-03 01:30:45", WithoutOffsetParser::YmdHMS)
                    .unwrap()
            else {
                panic!("expected ambiguous local datetime");
            };
            let offsets = [first.offset(), second.offset()];
            assert!(offsets.contains(&off(-4)) && offsets.contains(&off(-5)));
            assert_eq!(
                (first.unix_timestamp() - second.unix_timestamp()).abs(),
                3600
            );
        }

        // ---- format / shift ---------------------------------------------------

        #[test]
        #[serial]
        fn test_format_ext_local() {
            let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
            let dt = local(2024, Month::January, 5, 5, 45, 6, 789_123_456, off(0));
            assert_eq!(
                dt.format_ext_local(WithoutOffsetFormatter::YmdHMS.into())
                    .unwrap(),
                "2024-01-05 13:45:06"
            );
            assert_eq!(
                dt.format_ext_local(WithoutOffsetFormatter::YmdHMS3F.into())
                    .unwrap(),
                "2024-01-05 13:45:06.789"
            );
            assert_eq!(
                dt.format_ext_local(WithOffsetFormatter::YmdTHMS3F.into())
                    .unwrap(),
                "2024-01-05T13:45:06.789+08:00"
            );
        }

        #[test]
        #[serial]
        fn test_shift_local() {
            let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
            let dt = local(2024, Month::January, 5, 5, 45, 6, 0, off(0));
            let shifted = dt.shift_local(Duration::hours(8)).unwrap();
            // 结果转换到系统时区，与 UTC 表示同一时刻
            assert_eq!(shifted.offset(), off(8));
            assert_eq!(
                shifted,
                local(2024, Month::January, 5, 13, 45, 6, 0, off(0))
            );
            // 负向平移跨过本地零点
            let shifted = dt.shift_local(-Duration::hours(1)).unwrap();
            assert_eq!(shifted, local(2024, Month::January, 5, 4, 45, 6, 0, off(0)));
        }

        // ---- 本地日历边界 ------------------------------------------------------

        #[test]
        #[serial]
        fn test_boundaries_local_shanghai() {
            let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
            // 2024-01-03 是周三
            let dt = local(2024, Month::January, 3, 15, 30, 45, 0, off(8));

            assert_local_eq(
                unique(dt.start_of_day_local()),
                local(2024, Month::January, 3, 0, 0, 0, 0, off(8)),
            );
            assert_local_eq(
                unique(dt.end_of_day_local()),
                local(2024, Month::January, 3, 23, 59, 59, 999_999_999, off(8)),
            );
            // 2024-01-01 是周一
            assert_local_eq(
                unique(dt.start_of_week_local()),
                local(2024, Month::January, 1, 0, 0, 0, 0, off(8)),
            );
            assert_local_eq(
                unique(dt.end_of_week_local()),
                local(2024, Month::January, 7, 23, 59, 59, 999_999_999, off(8)),
            );
            assert_local_eq(
                unique(dt.start_of_month_local()),
                local(2024, Month::January, 1, 0, 0, 0, 0, off(8)),
            );
            assert_local_eq(
                unique(dt.end_of_month_local()),
                local(2024, Month::January, 31, 23, 59, 59, 999_999_999, off(8)),
            );
            assert_local_eq(
                unique(dt.start_of_year_local()),
                local(2024, Month::January, 1, 0, 0, 0, 0, off(8)),
            );
            assert_local_eq(
                unique(dt.end_of_year_local()),
                local(2024, Month::December, 31, 23, 59, 59, 999_999_999, off(8)),
            );

            // 2 月：闰年 29 天 / 平年 28 天
            let leap = local(2024, Month::February, 15, 12, 0, 0, 0, off(8));
            assert_local_eq(
                unique(leap.end_of_month_local()),
                local(2024, Month::February, 29, 23, 59, 59, 999_999_999, off(8)),
            );
            let common = local(2023, Month::February, 15, 12, 0, 0, 0, off(8));
            assert_local_eq(
                unique(common.end_of_month_local()),
                local(2023, Month::February, 28, 23, 59, 59, 999_999_999, off(8)),
            );
            // 30 天的月
            let april = local(2024, Month::April, 15, 12, 0, 0, 0, off(8));
            assert_local_eq(
                unique(april.end_of_month_local()),
                local(2024, Month::April, 30, 23, 59, 59, 999_999_999, off(8)),
            );
        }

        #[test]
        #[serial]
        fn test_boundaries_local_new_york() {
            let _tz_guard = TzGuard::new(tz("America/New_York"));
            // 7 月处于 EDT（-04:00），当地零点 = 04:00 UTC
            let july = local(2024, Month::July, 10, 8, 0, 0, 0, off(-4));
            let start = unique(july.start_of_day_local());
            assert_eq!(start, local(2024, Month::July, 10, 0, 0, 0, 0, off(-4)));
            assert_eq!(start, local(2024, Month::July, 10, 4, 0, 0, 0, off(0)));
            assert_eq!(start.offset(), off(-4));
            // 1 月处于 EST（-05:00），当地零点 = 05:00 UTC
            let january = local(2024, Month::January, 10, 7, 0, 0, 0, off(-5));
            let start = unique(january.start_of_day_local());
            assert_eq!(start, local(2024, Month::January, 10, 0, 0, 0, 0, off(-5)));
            assert_eq!(start, local(2024, Month::January, 10, 5, 0, 0, 0, off(0)));
            assert_eq!(start.offset(), off(-5));
        }
    }
}

#[cfg(feature = "datetime-iana")]
pub use iana::{HygieaOffsetDateTimeExt, now_local};

// ---- chrono 互转桥（feature = "datetime-chrono"）-----------------------------------

#[cfg(feature = "datetime-chrono")]
pub use chrono_bridge::*;

#[cfg(feature = "datetime-chrono")]
mod chrono_bridge {
    use crate::{BaseErr, HyErr, ResultExt, err};
    use time::UtcDateTime;

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
}
