//! `Time`（钟面时间，不带日期和 offset）的 Formatter / Parser 和扩展 `HygieaTimeExt`。
//!
//! `Time` 是哪个时区的钟面时间由取它的地方决定：`now_utc().time()` 是 UTC 的，`now_local().time()` 是系统时区的。
//! serde 读写模块 `hms`、`hms_range` 在 `serde_ext.rs`。

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

#[cfg(test)]
mod tests {
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
}
