// ---- formats ----------------------------------------------------------------

pub trait DateFormat: Send + Sync {
    fn pattern_time(&self) -> &'static [time::format_description::BorrowedFormatItem<'static>];

    fn pattern_chrono(&self) -> &'static [chrono::format::Item<'static>];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Formatter {
    /// %Y => 2024
    Y,
    /// %H:%M:%S => 13:30:00
    HMS,
    /// %Y-%m-%d => 2024-01-15
    Ymd,
    /// %Y%m%d => 20240115
    Ymdnosep,
    /// %Y-%m-%d %H:%M:%S => 2024-01-15 13:30:00
    YmdHMS,
    /// %Y-%m-%d %H:%M:%S.%.3f => 2024-01-15 13:30:00.026
    YmdHMS3F,
    /// %Y%m%d%H%M%S => 20240115133000
    YmdHMSnosep,
    /// ISO 8601 / RFC 3339 with timezone offset:
    ///   +08:00 => 2024-01-15T13:30:00+08:00
    ///   +00:00 => 2024-01-15T13:30:00+00:00
    ISO,
}

impl DateFormat for Formatter {
    fn pattern_time(&self) -> &'static [time::format_description::BorrowedFormatItem<'static>] {
        use time::macros::format_description;
        match self {
            Formatter::Y => format_description!("[year]"),
            Formatter::HMS => format_description!("[hour]:[minute]:[second]"),
            Formatter::Ymd => format_description!("[year]-[month]-[day]"),
            Formatter::Ymdnosep => format_description!("[year][month][day]"),
            Formatter::YmdHMS => format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
            Formatter::YmdHMS3F => format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"),
            Formatter::YmdHMSnosep => format_description!("[year][month][day][hour][minute][second]"),
            Formatter::ISO => format_description!("[year]-[month]-[day]T[hour]:[minute]:[second][offset_hour sign:mandatory]:[offset_minute]"),
        }
    }

    fn pattern_chrono(&self) -> &'static [chrono::format::Item<'static>] {
        use chrono::format::{Item, StrftimeItems};
        use std::sync::OnceLock;
        static Y: OnceLock<Vec<Item<'static>>> = OnceLock::new();
        static HMS: OnceLock<Vec<Item<'static>>> = OnceLock::new();
        static YMD: OnceLock<Vec<Item<'static>>> = OnceLock::new();
        static YMDNOSEP: OnceLock<Vec<Item<'static>>> = OnceLock::new();
        static YMDHMS: OnceLock<Vec<Item<'static>>> = OnceLock::new();
        static YMDHMS3F: OnceLock<Vec<Item<'static>>> = OnceLock::new();
        static YMDHMSNOSEP: OnceLock<Vec<Item<'static>>> = OnceLock::new();
        static ISO: OnceLock<Vec<Item<'static>>> = OnceLock::new();
        let (cell, pattern): (&OnceLock<Vec<Item<'static>>>, &'static str) = match self {
            Formatter::Y => (&Y, "%Y"),
            Formatter::HMS => (&HMS, "%T"),
            Formatter::Ymd => (&YMD, "%F"),
            Formatter::Ymdnosep => (&YMDNOSEP, "%Y%m%d"),
            Formatter::YmdHMS => (&YMDHMS, "%F %T"),
            Formatter::YmdHMS3F => (&YMDHMS3F, "%F %T%.3f"),
            Formatter::YmdHMSnosep => (&YMDHMSNOSEP, "%Y%m%d%H%M%S"),
            Formatter::ISO => (&ISO, "%+"),
        };
        cell.get_or_init(|| StrftimeItems::new(pattern).collect())
    }
}

// ---- time -------------------------------------------------------------------

pub use time_ext::*;

mod time_ext {
    use crate::{BaseFmtErr, FmtErr};
    use super::{DateFormat, Formatter};

    pub use time::OffsetDateTime;

    thread_local! {
        static NOW_FN: std::cell::Cell<fn() -> OffsetDateTime> = const { std::cell::Cell::new(OffsetDateTime::now_utc) };
    }

    #[cfg(test)]
    pub fn set_now_offset_utc(f: fn() -> OffsetDateTime) {
        NOW_FN.set(f);
    }

    pub fn now_offset_utc() -> OffsetDateTime {
        NOW_FN.with(|f| f.get()())
    }

    pub trait OffsetDateTimeExt {
        fn format(&self, fmt: &dyn DateFormat) -> Result<String, FmtErr>;
        fn format_default(&self) -> Result<String, FmtErr> {
            self.format(&Formatter::ISO)
        }

        fn parse(s: &str, fmt: &dyn DateFormat) -> Result<OffsetDateTime, FmtErr>;
        fn parse_default(s: &str) -> Result<OffsetDateTime, FmtErr> {
            Self::parse(s, &Formatter::ISO)
        }

        fn shift(&self, duration: time::Duration) -> OffsetDateTime;

        fn from_millis(ms: i64) -> Result<OffsetDateTime, FmtErr>;
        fn from_secs(s: i64) -> Result<OffsetDateTime, FmtErr>;

        fn start_of_day(&self) -> OffsetDateTime;
        fn end_of_day(&self) -> OffsetDateTime;
        fn start_of_week(&self) -> OffsetDateTime;
        fn start_of_month(&self) -> Result<OffsetDateTime, FmtErr>;
        fn start_of_year(&self) -> Result<OffsetDateTime, FmtErr>;
    }

    impl OffsetDateTimeExt for OffsetDateTime {
        fn format(&self, fmt: &dyn DateFormat) -> Result<String, FmtErr> {
            OffsetDateTime::format(*self, fmt.pattern_time()).map_err(|e| {
                BaseFmtErr::DateError.to_err(serde_json::json!({
                    "cause": format!("format datetime failed: {e}")
                }))
            })
        }
        fn parse(s: &str, fmt: &dyn DateFormat) -> Result<OffsetDateTime, FmtErr> {
            OffsetDateTime::parse(s, fmt.pattern_time()).map_err(|e| {
                BaseFmtErr::DateError.to_err(serde_json::json!({
                    "cause": format!("parse datetime failed: {e}, input={s}")
                }))
            })
        }
        fn shift(&self, duration: time::Duration) -> OffsetDateTime {
            *self + duration
        }
        fn from_millis(ms: i64) -> Result<OffsetDateTime, FmtErr> {
            OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).map_err(|e| {
                BaseFmtErr::DateError.to_err(
                    serde_json::json!({ "cause": format!("timestamp millis out of range: {ms}, {e}") }),
                )
            })
        }
        fn from_secs(s: i64) -> Result<OffsetDateTime, FmtErr> {
            OffsetDateTime::from_unix_timestamp(s).map_err(|e| {
                BaseFmtErr::DateError.to_err(
                    serde_json::json!({ "cause": format!("timestamp secs out of range: {s}, {e}") }),
                )
            })
        }
        fn start_of_day(&self) -> OffsetDateTime {
            self.date().with_time(time::Time::MIDNIGHT).assume_utc()
        }
        fn end_of_day(&self) -> OffsetDateTime {
            self.date().with_time(time::Time::MAX).assume_utc()
        }
        fn start_of_week(&self) -> OffsetDateTime {
            let weekday = self.weekday().number_from_monday();
            let monday = *self - time::Duration::days(weekday.into());
            monday.start_of_day()
        }
        fn start_of_month(&self) -> Result<OffsetDateTime, FmtErr> {
            self.replace_day(1)
                .map(|dt| dt.date().with_time(time::Time::MIDNIGHT).assume_utc())
                .map_err(|e| {
                    BaseFmtErr::DateError.to_err(serde_json::json!({
                        "cause": format!("start_of_month failed: {e}")
                    }))
                })
        }
        fn start_of_year(&self) -> Result<OffsetDateTime, FmtErr> {
            self.replace_month(time::Month::January)
                .and_then(|dt| dt.replace_day(1))
                .map(|dt| dt.date().with_time(time::Time::MIDNIGHT).assume_utc())
                .map_err(|e| {
                    BaseFmtErr::DateError.to_err(serde_json::json!({
                        "cause": format!("start_of_year failed: {e}")
                    }))
                })
        }
    }
}

// ---- chrono -----------------------------------------------------------------

pub use chrono_ext::*;

mod chrono_ext {
    use crate::{BaseFmtErr, FmtErr};
    use super::{DateFormat, Formatter};

    pub use chrono::{
        DateTime, Datelike, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeDelta, TimeZone,
        Timelike, Utc,
    };

    thread_local! {
        static NOW_FN: std::cell::Cell<fn() -> DateTime<Utc>> = const { std::cell::Cell::new(Utc::now) };
    }

    #[cfg(test)]
    pub fn set_now_utc(f: fn() -> DateTime<Utc>) {
        NOW_FN.set(f);
    }

    pub fn now_utc() -> DateTime<Utc> {
        NOW_FN.with(|f| f.get()())
    }

    pub trait DateTimeUtcExt {
        fn to_offset_datetime(&self) -> Result<time::OffsetDateTime, FmtErr>;
        fn from_offset_datetime(odt: time::OffsetDateTime) -> Result<DateTime<Utc>, FmtErr>;

        fn format(&self, fmt: &dyn DateFormat) -> String;
        fn format_default(&self) -> String {
            self.format(&Formatter::ISO)
        }

        fn parse(s: &str, fmt: &dyn DateFormat) -> Result<DateTime<Utc>, FmtErr>;
        fn parse_default(s: &str) -> Result<DateTime<Utc>, FmtErr> {
            Self::parse(s, &Formatter::ISO)
        }

        fn shift(&self, duration: TimeDelta) -> DateTime<Utc>;

        fn from_millis(ms: i64) -> Result<DateTime<Utc>, FmtErr>;
        fn from_secs(s: i64) -> Result<DateTime<Utc>, FmtErr>;

        fn start_of_day(&self) -> DateTime<Utc>;
        fn end_of_day(&self) -> DateTime<Utc>;
        fn start_of_week(&self) -> DateTime<Utc>;
        fn start_of_month(&self) -> Result<DateTime<Utc>, FmtErr>;
        fn start_of_year(&self) -> Result<DateTime<Utc>, FmtErr>;
    }

    impl DateTimeUtcExt for DateTime<Utc> {
        fn to_offset_datetime(&self) -> Result<time::OffsetDateTime, FmtErr> {
            let secs = self.timestamp();
            let nanos = self.timestamp_subsec_nanos();
            time::OffsetDateTime::from_unix_timestamp(secs)
                .map(|odt| odt + time::Duration::nanoseconds(nanos as i64))
                .map_err(|e| {
                    BaseFmtErr::DateError.to_err(serde_json::json!({
                        "cause": format!("to_offset_datetime out of range: {e}")
                    }))
                })
        }
        fn from_offset_datetime(odt: time::OffsetDateTime) -> Result<DateTime<Utc>, FmtErr> {
            DateTime::from_timestamp(odt.unix_timestamp(), odt.nanosecond()).ok_or_else(|| {
                BaseFmtErr::DateError.to_err(serde_json::json!({
                    "cause": "from_offset_datetime: timestamp out of range"
                }))
            })
        }
        fn format(&self, fmt: &dyn DateFormat) -> String {
            self.format_with_items(fmt.pattern_chrono().iter()).to_string()
        }
        fn parse(s: &str, fmt: &dyn DateFormat) -> Result<DateTime<Utc>, FmtErr> {
            use chrono::format::Parsed;
            let mut parsed = Parsed::new();
            chrono::format::parse(&mut parsed, s, fmt.pattern_chrono().iter())
                .and_then(|_| parsed.to_naive_datetime_with_offset(0))
                .map(|dt| dt.and_utc())
                .map_err(|e| {
                    BaseFmtErr::DateError.to_err(serde_json::json!({
                        "cause": format!("parse datetime failed: {e}, input={s}")
                    }))
                })
        }
        fn shift(&self, duration: TimeDelta) -> DateTime<Utc> {
            *self + duration
        }
        fn from_millis(ms: i64) -> Result<DateTime<Utc>, FmtErr> {
            DateTime::from_timestamp_millis(ms).ok_or_else(|| {
                BaseFmtErr::DateError.to_err(
                    serde_json::json!({ "cause": format!("timestamp millis out of range: {ms}") }),
                )
            })
        }
        fn from_secs(s: i64) -> Result<DateTime<Utc>, FmtErr> {
            DateTime::from_timestamp(s, 0).ok_or_else(|| {
                BaseFmtErr::DateError.to_err(
                    serde_json::json!({ "cause": format!("timestamp secs out of range: {s}") }),
                )
            })
        }
        fn start_of_day(&self) -> DateTime<Utc> {
            self.date_naive().and_time(NaiveDateTime::MIN.time()).and_utc()
        }
        fn end_of_day(&self) -> DateTime<Utc> {
            self.date_naive().and_time(NaiveDateTime::MAX.time()).and_utc()
        }
        fn start_of_week(&self) -> DateTime<Utc> {
            let date = self.date_naive();
            let weekday = date.weekday().num_days_from_monday();
            let monday = date - TimeDelta::days(weekday as i64);
            monday.and_time(NaiveDateTime::MIN.time()).and_utc()
        }
        fn start_of_month(&self) -> Result<DateTime<Utc>, FmtErr> {
            self.date_naive()
                .with_day(1)
                .map(|d| d.and_time(NaiveDateTime::MIN.time()).and_utc())
                .ok_or_else(|| {
                    BaseFmtErr::DateError.to_err(serde_json::json!({
                        "cause": "start_of_month failed"
                    }))
                })
        }
        fn start_of_year(&self) -> Result<DateTime<Utc>, FmtErr> {
            self.date_naive()
                .with_month(1)
                .and_then(|d| d.with_day(1))
                .map(|d| d.and_time(NaiveDateTime::MIN.time()).and_utc())
                .ok_or_else(|| {
                    BaseFmtErr::DateError.to_err(serde_json::json!({
                        "cause": "start_of_year failed"
                    }))
                })
        }
    }
}
