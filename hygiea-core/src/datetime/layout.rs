//! 格式布局：`DateTimeFormatter`、各 Formatter / Parser 枚举（同一个布局既能格式化也能解析），名字和 `FromStr` / `Display` / serde 互转。

use std::str::FromStr;

use time::format_description::BorrowedFormatItem;
use time::formatting::Formattable;
use time::macros::format_description;
use time::{OffsetDateTime, UtcDateTime};

use super::HygieaDateTimeExt;
use crate::{BaseErr, HyErr, ResultExt, err};

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

impl From<WithoutOffsetParser> for DateTimeFormatter {
    fn from(parser: WithoutOffsetParser) -> Self {
        Self::WithoutOffset(parser.into())
    }
}

impl FromStr for DateTimeFormatter {
    type Err = HyErr;

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
            _ => Err(err!(
                BaseErr::DateError,
                format!("unsupported date time formatter: {value}")
            )),
        }
    }
}

/// 输出和 `FromStr` 对称的名字，比如 `WithOffset.YmdHMS3F`
impl std::fmt::Display for DateTimeFormatter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::WithOffset(WithOffsetFormatter::YmdHMS3F) => "WithOffset.YmdHMS3F",
            Self::WithOffset(WithOffsetFormatter::YmdTHMS3F) => "WithOffset.YmdTHMS3F",
            Self::WithoutOffset(WithoutOffsetFormatter::Y) => "WithoutOffset.Y",
            Self::WithoutOffset(WithoutOffsetFormatter::HMS) => "WithoutOffset.HMS",
            Self::WithoutOffset(WithoutOffsetFormatter::Ymd) => "WithoutOffset.Ymd",
            Self::WithoutOffset(WithoutOffsetFormatter::Ymdnosep) => "WithoutOffset.Ymdnosep",
            Self::WithoutOffset(WithoutOffsetFormatter::YmdHMS) => "WithoutOffset.YmdHMS",
            Self::WithoutOffset(WithoutOffsetFormatter::YmdHMS3F) => "WithoutOffset.YmdHMS3F",
            Self::WithoutOffset(WithoutOffsetFormatter::YmdHMSnosep) => "WithoutOffset.YmdHMSnosep",
        };
        f.write_str(name)
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

#[cfg(feature = "log")]
impl serde::Serialize for DateTimeFormatter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
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
    pub(super) fn description(self) -> &'static [BorrowedFormatItem<'static>] {
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
    pub(super) fn description(self) -> &'static [BorrowedFormatItem<'static>] {
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
    pub(super) fn description(self) -> &'static [BorrowedFormatItem<'static>] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use time::UtcOffset;

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
            assert_eq!(expected.to_string(), input);
            #[cfg(feature = "log")]
            {
                let json = serde_json::to_string(&expected).unwrap();
                assert_eq!(json, format!("\"{input}\""));
                assert_eq!(
                    serde_json::from_str::<DateTimeFormatter>(&json).unwrap(),
                    expected
                );
            }
        }
        let error = "nonsense".parse::<DateTimeFormatter>().unwrap_err();
        assert!(error.is(BaseErr::DateError));
        assert!(error.to_string().contains("unsupported"));
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
        assert_eq!(
            DateTimeFormatter::from(WithoutOffsetParser::YmdHMSnosep),
            DateTimeFormatter::WithoutOffset(WithoutOffsetFormatter::YmdHMSnosep)
        );
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
}
