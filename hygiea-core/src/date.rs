// ---- formats ----------------------------------------------------------------

pub trait DateFormat: Send + Sync {
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
    /// ISO 8601 UTC with milliseconds and Z suffix:
    ///   2024-01-15T13:30:00.123Z
    IsoMillisZ,
}

impl DateFormat for Formatter {
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
        static ISO_MILLIS_Z: OnceLock<Vec<Item<'static>>> = OnceLock::new();
        let (cell, pattern): (&OnceLock<Vec<Item<'static>>>, &'static str) = match self {
            Formatter::Y => (&Y, "%Y"),
            Formatter::HMS => (&HMS, "%T"),
            Formatter::Ymd => (&YMD, "%F"),
            Formatter::Ymdnosep => (&YMDNOSEP, "%Y%m%d"),
            Formatter::YmdHMS => (&YMDHMS, "%F %T"),
            Formatter::YmdHMS3F => (&YMDHMS3F, "%F %T%.3f"),
            Formatter::YmdHMSnosep => (&YMDHMSNOSEP, "%Y%m%d%H%M%S"),
            Formatter::ISO => (&ISO, "%+"),
            Formatter::IsoMillisZ => (&ISO_MILLIS_Z, "%FT%T%.3fZ"),
        };
        cell.get_or_init(|| StrftimeItems::new(pattern).collect())
    }
}

// ---- chrono -----------------------------------------------------------------

pub use chrono_ext::*;

mod chrono_ext {
    use crate::{BaseFmtErr, FmtErr};
    use super::{DateFormat, Formatter};
    use parking_lot::RwLock;

    pub use chrono::{
        DateTime, Datelike, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeDelta, TimeZone,
        Timelike, Utc,
    };
    pub use time::OffsetDateTime;

    static NOW_FN: RwLock<fn() -> DateTime<Utc>> = RwLock::new(Utc::now);

    #[doc(hidden)]
    pub fn set_now_utc(f: fn() -> DateTime<Utc>) {
        *NOW_FN.write() = f;
    }

    pub fn now_utc() -> DateTime<Utc> {
        NOW_FN.read()()
    }

    pub trait OffsetDateTimeExt {
        fn to_datetime_utc(&self) -> Result<DateTime<Utc>, FmtErr>;
        fn from_datetime_utc(dt: DateTime<Utc>) -> Result<OffsetDateTime, FmtErr>;
    }

    impl OffsetDateTimeExt for OffsetDateTime {
        fn to_datetime_utc(&self) -> Result<DateTime<Utc>, FmtErr> {
            DateTime::from_timestamp(self.unix_timestamp(), self.nanosecond()).ok_or_else(|| {
                BaseFmtErr::DateError.to_err(serde_json::json!({
                    "cause": "to_datetime_utc: timestamp out of range"
                }))
            })
        }
        fn from_datetime_utc(dt: DateTime<Utc>) -> Result<OffsetDateTime, FmtErr> {
            let secs = dt.timestamp();
            let nanos = dt.timestamp_subsec_nanos();
            OffsetDateTime::from_unix_timestamp(secs)
                .map(|odt| odt + time::Duration::nanoseconds(nanos as i64))
                .map_err(|e| {
                    BaseFmtErr::DateError.to_err(serde_json::json!({
                        "cause": format!("from_datetime_utc out of range: {e}")
                    }))
                })
        }
    }

    pub trait DateTimeUtcExt {
        fn format_ext(&self, fmt: &dyn DateFormat) -> String;
        fn format_ext_default(&self) -> String {
            self.format_ext(&Formatter::ISO)
        }

        fn parse_ext(s: &str, fmt: &dyn DateFormat) -> Result<DateTime<Utc>, FmtErr>;
        fn parse_ext_default(s: &str) -> Result<DateTime<Utc>, FmtErr> {
            Self::parse_ext(s, &Formatter::ISO)
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
        fn format_ext(&self, fmt: &dyn DateFormat) -> String {
            self.format_with_items(fmt.pattern_chrono().iter()).to_string()
        }
        fn parse_ext(s: &str, fmt: &dyn DateFormat) -> Result<DateTime<Utc>, FmtErr> {
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
