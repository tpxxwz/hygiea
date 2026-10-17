//! `Time`（钟面时间，不带日期和 offset）的 Formatter / Parser、扩展 `HygieaTimeExt` 和 serde 读写模块 `hms`、`hms_range`。
//!
//! `Time` 是哪个时区的钟面时间由取它的地方决定：`now_utc().time()` 是 UTC 的，`now_local().time()` 是系统时区的。
//! 配置里的 `HH:MM:SS` 字段用 `hms`：
//!
//! ```ignore
//! #[derive(Serialize, Deserialize)]
//! struct Task {
//!     #[serde(with = "hygiea::datetime::hms")]
//!     start_time: Time,
//!     #[serde(default, with = "hygiea::datetime::hms::option")]
//!     end_time: Option<Time>,
//! }
//! ```
//!
//! 时间段 `Range<Time>` 存成 `{"start": .., "end": ..}`，两头按同一个 [`TimeParser`] 读写：`HMS` 用预先生成的 [`hms_range`]，
//! 别的格式用 [`time_range_serde!`](crate::time_range_serde) 生成。和标准库的 `Range` 一样左闭右开，不跨天：
//! 读入时 `start` 比 `end` 晚报错（比如想表示跨天的 `22:00..06:00`），`start == end` 是空区间。

use time::Time;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

use crate::{BaseErr, Result, ResultExt, err};

// ---- Formatter / Parser --------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormatter {
    HMS,
    HMSnosep,
    HM,
    HMnosep,
    H,
}

/// 解析时格式里没有的分、秒按 0 补：`HM` 的 `02:05`、`HMnosep` 的 `0205` 是 02:05:00，`H` 的 `02` 是 02:00:00
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeParser {
    HMS,
    HMSnosep,
    HM,
    HMnosep,
    H,
}

impl From<TimeParser> for TimeFormatter {
    fn from(parser: TimeParser) -> Self {
        match parser {
            TimeParser::HMS => Self::HMS,
            TimeParser::HMSnosep => Self::HMSnosep,
            TimeParser::HM => Self::HM,
            TimeParser::HMnosep => Self::HMnosep,
            TimeParser::H => Self::H,
        }
    }
}

impl TimeFormatter {
    pub(super) fn description(self) -> &'static [BorrowedFormatItem<'static>] {
        match self {
            Self::HMS => format_description!("[hour]:[minute]:[second]"),
            Self::HMSnosep => format_description!("[hour][minute][second]"),
            Self::HM => format_description!("[hour]:[minute]"),
            Self::HMnosep => format_description!("[hour][minute]"),
            Self::H => format_description!("[hour]"),
        }
    }
}

impl TimeParser {
    pub(super) fn description(self) -> &'static [BorrowedFormatItem<'static>] {
        TimeFormatter::from(self).description()
    }

    pub fn parse(self, input: &str) -> Result<Time> {
        Time::parse(input, self.description()).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("parse time failed, input={input}")
            )
        })
    }
}

// ---- HygieaTimeExt -------------------------------------------------------

/// `Time` 的扩展：按 [`TimeParser`] 解析、按 [`TimeFormatter`] 输出
pub trait HygieaTimeExt: Sized {
    /// 按指定格式解析
    fn parse_ext(s: &str, parser: TimeParser) -> Result<Self>;
    /// 按指定格式输出
    fn format_ext(&self, formatter: TimeFormatter) -> Result<String>;
}

impl HygieaTimeExt for Time {
    fn parse_ext(s: &str, parser: TimeParser) -> Result<Self> {
        parser.parse(s)
    }

    fn format_ext(&self, formatter: TimeFormatter) -> Result<String> {
        self.format(formatter.description())
            .wrap_err(|| err!(BaseErr::DateError, "format time failed"))
    }
}

// ---- serde ---------------------------------------------------------------

// serde 用的 `HH:MM:SS` 读写模块 `hms` 和 `hms::option`，格式同 `TimeParser::HMS`
time::serde::format_description!(pub hms, Time, "[hour]:[minute]:[second]");

// serde 用的 `Range<Time>` 读写模块 `hms_range`，两头都按 `TimeParser::HMS`：`{"start":"02:00:00","end":"06:00:00"}`
crate::time_range_serde!(pub hms_range, crate::datetime::TimeParser::HMS);

/// 生成 `Range<Time>` 的 serde 读写模块：存成 `{"start": .., "end": ..}`，两头按同一个 [`TimeParser`] 读写
///
/// ```ignore
/// hygiea::time_range_serde!(hm_range, hygiea::datetime::TimeParser::HM);
///
/// #[derive(Serialize, Deserialize)]
/// struct Task {
///     #[serde(with = "hm_range")]
///     window: Range<Time>,
/// }
/// ```
#[macro_export]
macro_rules! time_range_serde {
    ($vis:vis $name:ident, $parser:expr) => {
        $vis mod $name {
            #[allow(unused_imports)]
            use super::*;

            pub fn serialize<S>(
                range: &::std::ops::Range<$crate::datetime::Time>,
                serializer: S,
            ) -> ::std::result::Result<S::Ok, S::Error>
            where
                S: $crate::datetime::__serde::Serializer,
            {
                $crate::datetime::__time_range::serialize(range, $parser, serializer)
            }

            pub fn deserialize<'de, D>(
                deserializer: D,
            ) -> ::std::result::Result<::std::ops::Range<$crate::datetime::Time>, D::Error>
            where
                D: $crate::datetime::__serde::Deserializer<'de>,
            {
                $crate::datetime::__time_range::deserialize(deserializer, $parser)
            }
        }
    };
}

/// [`time_range_serde!`] 生成的模块调用的实现
#[doc(hidden)]
pub mod __time_range {
    use std::ops::Range;

    use serde::de::Error as _;
    use serde::ser::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use time::Time;

    use super::{HygieaTimeExt, TimeParser};
    use crate::{BaseErr, err};

    pub fn serialize<S: Serializer>(
        range: &Range<Time>,
        parser: TimeParser,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let start = range
            .start
            .format_ext(parser.into())
            .map_err(S::Error::custom)?;
        let end = range
            .end
            .format_ext(parser.into())
            .map_err(S::Error::custom)?;
        (start..end).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
        parser: TimeParser,
    ) -> Result<Range<Time>, D::Error> {
        let range = Range::<String>::deserialize(deserializer)?;
        let start = parser.parse(&range.start).map_err(D::Error::custom)?;
        let end = parser.parse(&range.end).map_err(D::Error::custom)?;
        if start > end {
            return Err(D::Error::custom(err!(
                BaseErr::DateError,
                format!("range start is after end: start={start}, end={end}")
            )));
        }
        Ok(start..end)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;

    // ---- HygieaTimeExt ---------------------------------------------------

    #[test]
    fn test_time_parse_and_format() {
        let time = Time::parse_ext("02:05:09", TimeParser::HMS).unwrap();
        assert_eq!(time, Time::from_hms(2, 5, 9).unwrap());
        assert_eq!(time.format_ext(TimeFormatter::HMS).unwrap(), "02:05:09");
    }

    #[test]
    fn test_time_parse_and_format_nosep() {
        let time = Time::parse_ext("020509", TimeParser::HMSnosep).unwrap();
        assert_eq!(time, Time::from_hms(2, 5, 9).unwrap());
        assert_eq!(time.format_ext(TimeFormatter::HMSnosep).unwrap(), "020509");
    }

    #[test]
    fn test_time_parse_nosep_rejects_invalid() {
        for input in ["20509", "0205", "240000", "02:05:09", ""] {
            let error = Time::parse_ext(input, TimeParser::HMSnosep).unwrap_err();
            assert!(error.is(BaseErr::DateError), "{input}");
        }
    }

    #[test]
    fn test_time_parse_fills_missing_with_zero() {
        let time = Time::parse_ext("02:05", TimeParser::HM).unwrap();
        assert_eq!(time, Time::from_hms(2, 5, 0).unwrap());
        let time = Time::parse_ext("0205", TimeParser::HMnosep).unwrap();
        assert_eq!(time, Time::from_hms(2, 5, 0).unwrap());
        let time = Time::parse_ext("02", TimeParser::H).unwrap();
        assert_eq!(time, Time::from_hms(2, 0, 0).unwrap());
    }

    #[test]
    fn test_time_format_hm_hmnosep_h() {
        let time = Time::from_hms(2, 5, 9).unwrap();
        assert_eq!(time.format_ext(TimeFormatter::HM).unwrap(), "02:05");
        assert_eq!(time.format_ext(TimeFormatter::HMnosep).unwrap(), "0205");
        assert_eq!(time.format_ext(TimeFormatter::H).unwrap(), "02");
    }

    #[test]
    fn test_time_parse_hm_hmnosep_h_reject_invalid() {
        for input in ["2:05", "02:05:09", "24:00", "0205", ""] {
            let error = Time::parse_ext(input, TimeParser::HM).unwrap_err();
            assert!(error.is(BaseErr::DateError), "{input}");
        }
        for input in ["205", "2400", "02:05", "020509", ""] {
            let error = Time::parse_ext(input, TimeParser::HMnosep).unwrap_err();
            assert!(error.is(BaseErr::DateError), "{input}");
        }
        for input in ["2", "24", "02:05", ""] {
            let error = Time::parse_ext(input, TimeParser::H).unwrap_err();
            assert!(error.is(BaseErr::DateError), "{input}");
        }
    }

    #[test]
    fn test_time_parse_rejects_invalid() {
        for input in ["2:05:09", "02:05", "24:00:00", "02:05:09.5", ""] {
            let error = Time::parse_ext(input, TimeParser::HMS).unwrap_err();
            assert!(error.is(BaseErr::DateError), "{input}");
        }
    }

    // ---- serde ---------------------------------------------------------------

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct HmsFields {
        #[serde(with = "hms")]
        at: Time,
        #[serde(default, with = "hms::option")]
        maybe: Option<Time>,
    }

    #[test]
    fn hms_reads_and_writes() {
        let fields: HmsFields = serde_json::from_str(r#"{"at":"02:05:09"}"#).unwrap();
        let expected = Time::from_hms(2, 5, 9).unwrap();
        assert_eq!(fields.at, expected);
        assert_eq!(fields.maybe, None);
        let fields = HmsFields {
            at: expected,
            maybe: Some(expected),
        };
        assert_eq!(
            serde_json::to_string(&fields).unwrap(),
            r#"{"at":"02:05:09","maybe":"02:05:09"}"#
        );
    }

    #[test]
    fn hms_rejects_invalid() {
        for at in ["\"2:05:09\"", "\"02:05\"", "\"\""] {
            let json = format!(r#"{{"at":{at}}}"#);
            assert!(serde_json::from_str::<HmsFields>(&json).is_err(), "{json}");
        }
    }

    // ---- serde：Range<Time> ------------------------------------------------

    crate::time_range_serde!(hm_range, TimeParser::HM);

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct RangeFields {
        #[serde(with = "hms_range")]
        hms: Range<Time>,
        #[serde(with = "hm_range")]
        hm: Range<Time>,
    }

    #[test]
    fn time_range_reads_and_writes() {
        let fields = RangeFields {
            hms: Time::from_hms(2, 0, 0).unwrap()..Time::from_hms(6, 30, 15).unwrap(),
            hm: Time::from_hms(8, 0, 0).unwrap()..Time::from_hms(22, 30, 0).unwrap(),
        };
        let json = serde_json::to_string(&fields).unwrap();
        assert_eq!(
            json,
            r#"{"hms":{"start":"02:00:00","end":"06:30:15"},"hm":{"start":"08:00","end":"22:30"}}"#
        );
        assert_eq!(serde_json::from_str::<RangeFields>(&json).unwrap(), fields);
    }

    #[test]
    fn time_range_rejects_invalid() {
        for json in [
            // 格式和 Parser 对不上
            r#"{"hms":{"start":"02:00","end":"06:00:00"},"hm":{"start":"08:00","end":"22:00"}}"#,
            r#"{"hms":{"start":"02:00:00","end":"06:00:00"},"hm":{"start":"08:00:00","end":"22:00"}}"#,
            // 缺一头
            r#"{"hms":{"start":"02:00:00"},"hm":{"start":"08:00","end":"22:00"}}"#,
            // start 比 end 晚
            r#"{"hms":{"start":"22:00:00","end":"06:00:00"},"hm":{"start":"08:00","end":"22:00"}}"#,
        ] {
            assert!(serde_json::from_str::<RangeFields>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn time_range_allows_empty() {
        let json =
            r#"{"hms":{"start":"02:00:00","end":"02:00:00"},"hm":{"start":"08:00","end":"08:00"}}"#;
        let fields = serde_json::from_str::<RangeFields>(json).unwrap();
        assert!(fields.hms.is_empty());
    }
}
