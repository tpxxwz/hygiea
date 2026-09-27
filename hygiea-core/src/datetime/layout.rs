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
    use time::macros::datetime;
    use time::{PrimitiveDateTime, UtcOffset};

    use super::*;

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

    // ---- 解析失败 --------------------------------------------------------------

    #[test]
    fn test_parse_invalid_calendar_date_fails() {
        // 2023 不是闰年，没有 2 月 29 日
        assert!(
            PrimitiveDateTime::parse(
                "2023-02-29 12:00:00",
                WithoutOffsetFormatter::from(WithoutOffsetParser::YmdHMS).description(),
            )
            .is_err()
        );
    }

    #[test]
    fn test_parse_invalid_time_fails() {
        // 小时只能是 0-23
        assert!(
            PrimitiveDateTime::parse(
                "2024-01-05 24:00:00",
                WithoutOffsetFormatter::from(WithoutOffsetParser::YmdHMS).description(),
            )
            .is_err()
        );
    }

    #[test]
    fn test_parse_trailing_characters_fails() {
        assert!(
            PrimitiveDateTime::parse(
                "2024-01-05 13:45:06x",
                WithoutOffsetFormatter::from(WithoutOffsetParser::YmdHMS).description(),
            )
            .is_err()
        );
    }

    #[test]
    fn test_parse_empty_input_fails() {
        assert!(
            PrimitiveDateTime::parse(
                "",
                WithoutOffsetFormatter::from(WithoutOffsetParser::YmdHMS).description(),
            )
            .is_err()
        );
    }

    #[test]
    fn test_parse_subsecond_digits_insufficient_fails() {
        // `[subsecond digits:3]` 要求恰好 3 位，只给 2 位报错
        assert!(
            PrimitiveDateTime::parse(
                "2024-01-05 13:45:06.78",
                WithoutOffsetFormatter::from(WithoutOffsetParser::YmdHMS3F).description(),
            )
            .is_err()
        );
    }

    #[test]
    fn test_parse_nosep_insufficient_digits_fails() {
        // 少一位秒的十位数字（正常应是 20240105134506，14 位）
        assert!(
            PrimitiveDateTime::parse(
                "2024010513450",
                WithoutOffsetFormatter::from(WithoutOffsetParser::YmdHMSnosep).description(),
            )
            .is_err()
        );
    }

    #[test]
    fn test_parse_offset_missing_leading_zero_fails() {
        // `[offset_hour sign:mandatory]` 按 2 位补零，"+8:00" 缺少前导 0
        assert!(
            OffsetDateTime::parse(
                "2024-01-05 21:45:06 +8:00",
                WithOffsetParser::YmdHMS.description()
            )
            .is_err()
        );
    }

    // ---- format → parse 往返 ----------------------------------------------------

    /// `WithoutOffsetParser` 的每个变体都做一遍格式化再解析，应该等于原值
    #[test]
    fn test_without_offset_parser_format_parse_round_trip() {
        let cases = [
            (WithoutOffsetParser::YmdHMS, datetime!(2024-01-05 13:45:06)),
            (
                WithoutOffsetParser::YmdHMS3F,
                datetime!(2024-01-05 13:45:06.789),
            ),
            (
                WithoutOffsetParser::YmdHMSnosep,
                datetime!(2024-01-05 13:45:06),
            ),
        ];
        for (parser, dt) in cases {
            let description = WithoutOffsetFormatter::from(parser).description();
            let formatted = dt.format(description).unwrap();
            let parsed = PrimitiveDateTime::parse(&formatted, description).unwrap();
            assert_eq!(parsed, dt, "{parser:?}");
        }
    }

    /// `WithOffsetParser` 的每个变体都做一遍格式化再解析，应该等于原值
    #[test]
    fn test_with_offset_parser_format_parse_round_trip() {
        let cases = [
            (WithOffsetParser::YmdHMS, datetime!(2024-01-05 21:45:06 +8)),
            (
                WithOffsetParser::YmdHMS3F,
                datetime!(2024-01-05 21:45:06.789 +8),
            ),
            (
                WithOffsetParser::YmdHMSnosep,
                datetime!(2024-01-05 21:45:06 +8),
            ),
        ];
        for (parser, dt) in cases {
            let description = parser.description();
            let formatted = dt.format(description).unwrap();
            let parsed = OffsetDateTime::parse(&formatted, description).unwrap();
            assert_eq!(parsed, dt, "{parser:?}");
        }
    }

    /// `WithoutOffsetFormatter` 里 `Y` / `HMS` / `Ymd` / `Ymdnosep` 只能格式化，没有对应的
    /// `WithoutOffsetParser` 变体；用穷尽 match 固化下来，新增变体时这里也要跟着改
    #[test]
    fn test_formatter_only_variants_have_no_parser() {
        use WithoutOffsetFormatter::*;

        let has_parser = |formatter: WithoutOffsetFormatter| {
            [
                WithoutOffsetParser::YmdHMS,
                WithoutOffsetParser::YmdHMS3F,
                WithoutOffsetParser::YmdHMSnosep,
            ]
            .into_iter()
            .any(|parser| WithoutOffsetFormatter::from(parser) == formatter)
        };

        for formatter in [Y, HMS, Ymd, Ymdnosep, YmdHMS, YmdHMS3F, YmdHMSnosep] {
            let expect_parser = matches!(formatter, YmdHMS | YmdHMS3F | YmdHMSnosep);
            assert_eq!(has_parser(formatter), expect_parser, "{formatter:?}");
        }

        // `WithOffsetFormatter::YmdTHMS3F` 同样只能格式化，但 WithOffsetParser 和 WithOffsetFormatter
        // 之间没有 `From` 转换可用来做同样的断言：这是两个独立枚举，YmdTHMS3F 只在 Formatter 里有变体，
        // Parser 没有同名变体纯粹是枚举定义的事实，没有运行时 API 能表达，这里不补充断言
    }

    // ---- 反序列化 --------------------------------------------------------------

    #[cfg(feature = "log")]
    #[test]
    fn test_datetime_formatter_deserialize_invalid_string_fails() {
        let err = serde_json::from_str::<DateTimeFormatter>("\"bogus\"").unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }
}
