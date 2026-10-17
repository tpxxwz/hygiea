//! `UtcDateTime` / `OffsetDateTime`：Formatter / Parser 和格式名、两种类型共有的扩展 `HygieaDateTimeExt`
//! （时间戳、RFC 3339、格式化、平移）、`UtcDateTime` 的 `HygieaUtcDateTimeExt`（UTC 日历边界）、serde 读写模块。
//! 系统时区和 `OffsetDateTime` 的 `HygieaOffsetDateTimeExt` 在 `local.rs`（feature `datetime-iana`）。
//!
//! # 格式化时注意 offset
//!
//! `format_ext` 按值自身的 offset 输出。配不带 offset 的格式（[`WithoutOffsetFormatter`]）时，
//! 字符串里看不出是哪个 offset 的钟面时间：
//!
//! ```ignore
//! let dt = now();                                        // UTC offset
//! dt.format_ext(WithoutOffsetFormatter::YmdHMS)?;        // UTC 钟面时间，不是本地时间
//! dt.format_ext_local(WithoutOffsetFormatter::YmdHMS)?;  // 系统时区的钟面时间
//! dt.format_ext(WithOffsetFormatter::YmdTHMS3F)?;        // 带 offset，没有歧义
//! ```
//!
//! 要本地时间字符串用 `format_ext_local`；只有确定值自身的 offset 就是想要的，才用 `format_ext` 配不带 offset 的格式。
//!
//! # 格式名
//!
//! [`DateTimeFormatter`] 可以从配置里的名字解析（`FromStr` / serde），名字就是 `类别.变体`，
//! 比如 `WithOffset.YmdTHMS3F`、`WithoutOffset.YmdHMS`；[`WithoutOffsetParser`] 也能直接当格式用。
//!
//! # 日历边界
//!
//! - `start_of_*` / `end_of_*`：`end_of_*` 是闭区间终点（最后 1 纳秒），适合展示；
//! - 做区间判断用半开区间 `[start_of_x, start_of_next_x)`，不要写 `<= end_of_x`：
//!
//! ```
//! use hygiea_core::datetime::{HygieaUtcDateTimeExt, now_utc};
//!
//! let t = now_utc();
//! let (start, next) = (t.start_of_day(), t.start_of_next_day().unwrap());
//! assert!(start <= t && t < next);
//! ```
//!
//! - `_local` 版本返回 `OffsetResult`：夏令时切换可能让某天的零点**不存在**（`None`）或**出现两次**（`Ambiguous`），
//!   库不替调用方选，按业务自己处理：
//!
//! ```ignore
//! match dt.start_of_day_local()? {
//!     OffsetResult::Some(t) => t,
//!     OffsetResult::Ambiguous(first, second) => first.min(second), // 比如取较早的
//!     OffsetResult::None => { /* 比如跳过这一天，或取切换后的第一个时刻 */ }
//! }
//! ```
//!
//! # serde
//!
//! 外部接口常用 RFC 3339（`2026-10-05T12:57:24.719Z`），time 给 `UtcDateTime` 默认的 serde 格式却是
//! `2026-10-05 12:57:24.719`，读不了。字段上用 [`rfc3339_utc`]：解析时带 `Z` 或任意 offset 都换算成 UTC，
//! 输出带 `Z`；`Option` 字段用 `rfc3339_utc::option`，字段可能缺失时再加 `#[serde(default)]`：
//!
//! ```ignore
//! #[derive(Serialize, Deserialize)]
//! struct Card {
//!     #[serde(with = "hygiea::datetime::rfc3339_utc")]
//!     created_time: UtcDateTime,
//!     #[serde(default, with = "hygiea::datetime::rfc3339_utc::option")]
//!     expire_time: Option<UtcDateTime>,
//! }
//! ```
//!
//! 时间段 `Range<UtcDateTime>` / `Range<OffsetDateTime>` 存成 `{"start": .., "end": ..}`，两头按同一个 [`WithOffsetParser`] 读写：
//! - 写出同 `format_ext`：`UtcDateTime` 是 `+00:00`，`OffsetDateTime` 是值自身的 offset；
//! - 读入时字符串里必须带 offset，什么 offset 都行：`UtcDateTime` 换算成 UTC，`OffsetDateTime` 保留字符串里的 offset；
//! - 预先生成了 `YmdHMS` 的 [`ymdhms_utc_range`]、[`ymdhms_offset_range`]，别的格式用
//!   [`utc_range_serde!`](crate::utc_range_serde) / [`offset_range_serde!`](crate::offset_range_serde) 生成；
//! - 按系统时区写出（`2026-10-17 10:00:00 +08:00`）用 `_local` 的版本，在 `local.rs`；
//! - 读入时 `start` 比 `end` 晚报错（按时刻比，和 offset 无关），`start == end` 是空区间。

use std::str::FromStr;

use time::format_description::BorrowedFormatItem;
use time::format_description::well_known::Rfc3339;
use time::formatting::Formattable;
use time::macros::format_description;
use time::{Duration, Month, OffsetDateTime, Time, UtcDateTime};

use crate::{BaseErr, HyErr, Result, ResultExt, err};

// ---- Formatter / Parser --------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithOffsetFormatter {
    YmdHMS,
    YmdHMS3F,
    YmdHMSnosep,
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

impl From<WithOffsetParser> for WithOffsetFormatter {
    fn from(parser: WithOffsetParser) -> Self {
        match parser {
            WithOffsetParser::YmdHMS => Self::YmdHMS,
            WithOffsetParser::YmdHMS3F => Self::YmdHMS3F,
            WithOffsetParser::YmdHMSnosep => Self::YmdHMSnosep,
        }
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

impl WithOffsetFormatter {
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
            Self::YmdTHMS3F => format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3][offset_hour sign:mandatory]:[offset_minute]"
            ),
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

impl WithOffsetParser {
    pub(super) fn description(self) -> &'static [BorrowedFormatItem<'static>] {
        WithOffsetFormatter::from(self).description()
    }

    pub fn parse(self, input: &str) -> Result<OffsetDateTime> {
        OffsetDateTime::parse(input, self.description()).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("parse datetime with offset failed, input={input}")
            )
        })
    }
}

// ---- DateTimeFormatter ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateTimeFormatter {
    WithOffset(WithOffsetFormatter),
    WithoutOffset(WithoutOffsetFormatter),
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

impl From<WithOffsetParser> for DateTimeFormatter {
    fn from(parser: WithOffsetParser) -> Self {
        Self::WithOffset(parser.into())
    }
}

impl From<WithoutOffsetParser> for DateTimeFormatter {
    fn from(parser: WithoutOffsetParser) -> Self {
        Self::WithoutOffset(parser.into())
    }
}

impl Default for DateTimeFormatter {
    fn default() -> Self {
        Self::WithOffset(WithOffsetFormatter::YmdTHMS3F)
    }
}

impl FromStr for DateTimeFormatter {
    type Err = HyErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "WithOffset.YmdHMS" => Ok(Self::WithOffset(WithOffsetFormatter::YmdHMS)),
            "WithOffset.YmdHMS3F" => Ok(Self::WithOffset(WithOffsetFormatter::YmdHMS3F)),
            "WithOffset.YmdHMSnosep" => Ok(Self::WithOffset(WithOffsetFormatter::YmdHMSnosep)),
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
            Self::WithOffset(WithOffsetFormatter::YmdHMS) => "WithOffset.YmdHMS",
            Self::WithOffset(WithOffsetFormatter::YmdHMS3F) => "WithOffset.YmdHMS3F",
            Self::WithOffset(WithOffsetFormatter::YmdHMSnosep) => "WithOffset.YmdHMSnosep",
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

impl DateTimeFormatter {
    pub fn format<T: HygieaDateTimeExt>(self, datetime: &T) -> Result<String> {
        HygieaDateTimeExt::format_ext(datetime, self)
    }
}

// ---- HygieaDateTimeExt ---------------------------------------------------

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

/// UTC/绝对时间相关的通用日期时间扩展。
///
/// `OffsetDateTime` 与 `UtcDateTime` 都实现本 trait；返回日期时间的方法保持实现类型。
pub trait HygieaDateTimeExt: Sized + DateTimeFormattable {
    /// 从 Unix 毫秒时间戳创建日期时间。
    fn from_millis(ms: i64) -> Result<Self>;
    /// 从 Unix 秒时间戳创建日期时间。
    fn from_secs(s: i64) -> Result<Self>;

    /// 解析携带 offset 的 RFC 3339；时区/offset 语义由实现类型决定。
    fn parse_ext_rfc3339(s: &str) -> Result<Self>;
    /// 按实现类型自身的时区/offset 语义输出 RFC 3339。
    fn format_ext_rfc3339(&self) -> Result<String> {
        self.format_with(&Rfc3339)
            .wrap_err(|| err!(BaseErr::DateError, "format RFC 3339 datetime failed"))
    }
    /// 按指定格式输出日期时间，钟面时间按值自身的 offset 算。
    ///
    /// 配不带 offset 的格式时，字符串里没有 offset：`OffsetDateTime` 不一定是系统时区（比如 [`now()`](super::now)
    /// 是 UTC），要本地时间用 `format_ext_local`。
    fn format_ext(&self, formatter: impl Into<DateTimeFormatter>) -> Result<String> {
        let result = match formatter.into() {
            DateTimeFormatter::WithOffset(formatter) => self.format_with(formatter.description()),
            DateTimeFormatter::WithoutOffset(formatter) => {
                self.format_with(formatter.description())
            }
        };
        result.wrap_err(|| err!(BaseErr::DateError, "format datetime failed"))
    }

    /// 在绝对时间线上平移指定时长，`OffsetDateTime` 保留自身的 offset。越界返回 `DateError`
    fn shift(&self, duration: Duration) -> Result<Self>;
}

impl HygieaDateTimeExt for UtcDateTime {
    fn from_millis(ms: i64) -> Result<Self> {
        UtcDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("timestamp millis out of range: {ms}")
            )
        })
    }

    fn from_secs(s: i64) -> Result<Self> {
        UtcDateTime::from_unix_timestamp(s).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("timestamp secs out of range: {s}")
            )
        })
    }

    fn parse_ext_rfc3339(s: &str) -> Result<Self> {
        UtcDateTime::parse(s, &Rfc3339).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("parse RFC 3339 UTC datetime failed, input={s}")
            )
        })
    }

    fn shift(&self, duration: Duration) -> Result<Self> {
        self.checked_add(duration).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("shift datetime out of range: datetime={self}, duration={duration}")
            )
        })
    }
}

impl HygieaDateTimeExt for OffsetDateTime {
    fn from_millis(ms: i64) -> Result<OffsetDateTime> {
        OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("timestamp millis out of range: {ms}")
            )
        })
    }

    fn from_secs(s: i64) -> Result<OffsetDateTime> {
        OffsetDateTime::from_unix_timestamp(s).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("timestamp secs out of range: {s}")
            )
        })
    }

    fn parse_ext_rfc3339(s: &str) -> Result<OffsetDateTime> {
        OffsetDateTime::parse(s, &Rfc3339).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("parse RFC 3339 datetime failed, input={s}")
            )
        })
    }

    fn shift(&self, duration: Duration) -> Result<OffsetDateTime> {
        self.checked_add(duration).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("shift datetime out of range: datetime={self}, duration={duration}")
            )
        })
    }
}

// ---- HygieaUtcDateTimeExt ------------------------------------------------

/// `UtcDateTime` 专有的 UTC 日历边界扩展。
pub trait HygieaUtcDateTimeExt: HygieaDateTimeExt {
    // ---- 日边界 ----------------------------------------------------------------

    /// 返回当前日期的开始。不会失败
    fn start_of_day(&self) -> Self;
    /// 返回当前日期的结束（23:59:59.999999999，time 的精度是 1 纳秒，这就是当天最后一个时刻）。不会失败。
    /// 做区间判断用半开区间 `[start_of_day, start_of_next_day)`
    fn end_of_day(&self) -> Self;
    /// 返回下一天的开始。
    fn start_of_next_day(&self) -> Result<Self>;

    // ---- 周边界 ----------------------------------------------------------------

    /// 返回当前周的开始，周一为一周第一天。
    fn start_of_week(&self) -> Result<Self>;
    /// 返回当前周的结束，周日为一周最后一天。做区间判断用半开区间 `[start_of_week, start_of_next_week)`
    fn end_of_week(&self) -> Result<Self>;
    /// 返回下一周（下周一）的开始。
    fn start_of_next_week(&self) -> Result<Self>;

    // ---- 月边界 ----------------------------------------------------------------

    /// 返回当前月的开始。
    fn start_of_month(&self) -> Result<Self>;
    /// 返回当前月的结束。做区间判断用半开区间 `[start_of_month, start_of_next_month)`
    fn end_of_month(&self) -> Result<Self>;
    /// 返回下个月的开始。
    fn start_of_next_month(&self) -> Result<Self>;

    // ---- 年边界 ----------------------------------------------------------------

    /// 返回当前年的开始。
    fn start_of_year(&self) -> Result<Self>;
    /// 返回当前年的结束。做区间判断用半开区间 `[start_of_year, start_of_next_year)`
    fn end_of_year(&self) -> Result<Self>;
    /// 返回下一年的开始。
    fn start_of_next_year(&self) -> Result<Self>;
}

impl HygieaUtcDateTimeExt for UtcDateTime {
    fn start_of_day(&self) -> Self {
        UtcDateTime::new(self.date(), Time::MIDNIGHT)
    }

    fn end_of_day(&self) -> Self {
        UtcDateTime::new(self.date(), Time::MAX)
    }

    fn start_of_next_day(&self) -> Result<Self> {
        let date = self.date().next_day().ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("start_of_next_day out of range: datetime={self}")
            )
        })?;
        Ok(UtcDateTime::new(date, Time::MIDNIGHT))
    }

    fn start_of_week(&self) -> Result<Self> {
        let midnight = self.start_of_day();
        let days = self.weekday().number_days_from_monday() as i64;
        midnight.checked_sub(Duration::days(days)).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("start_of_week out of range: datetime={self}")
            )
        })
    }

    fn end_of_week(&self) -> Result<Self> {
        self.start_of_week()?
            .checked_add(Duration::days(7) - Duration::nanoseconds(1))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_week out of range: datetime={self}")
                )
            })
    }

    fn start_of_next_week(&self) -> Result<Self> {
        self.start_of_week()?
            .checked_add(Duration::days(7))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("start_of_next_week out of range: datetime={self}")
                )
            })
    }

    fn start_of_month(&self) -> Result<Self> {
        let date = self
            .date()
            .replace_day(1)
            .wrap_err(|| err!(BaseErr::DateError, "start_of_month failed"))?;
        Ok(UtcDateTime::new(date, Time::MIDNIGHT))
    }

    fn end_of_month(&self) -> Result<Self> {
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

    fn start_of_next_month(&self) -> Result<Self> {
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

    fn start_of_year(&self) -> Result<Self> {
        let date = self
            .date()
            .replace_day(1)
            .and_then(|date| date.replace_month(Month::January))
            .wrap_err(|| err!(BaseErr::DateError, "start_of_year failed"))?;
        Ok(UtcDateTime::new(date, Time::MIDNIGHT))
    }

    fn end_of_year(&self) -> Result<Self> {
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

    fn start_of_next_year(&self) -> Result<Self> {
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

// ---- serde ---------------------------------------------------------------

// serde 用的 RFC 3339 读写模块 `rfc3339_utc` 和 `rfc3339_utc::option`，说明见模块文档「serde」
time::serde::format_description!(pub rfc3339_utc, UtcDateTime, Rfc3339);

// serde 用的 `Range<UtcDateTime>` 读写模块 `ymdhms_utc_range`，两头都按 `WithOffsetParser::YmdHMS`：
// 写出 UTC（`+00:00`），读入任意 offset 换算成 UTC
crate::utc_range_serde!(pub ymdhms_utc_range, crate::datetime::WithOffsetParser::YmdHMS);

// 同 `ymdhms_utc_range`，读写 `Range<OffsetDateTime>`：按值自身的 offset 写出，读入保留字符串里的 offset
crate::offset_range_serde!(pub ymdhms_offset_range, crate::datetime::WithOffsetParser::YmdHMS);
/// 生成 `Range<UtcDateTime>` 的 serde 读写模块：存成 `{"start": .., "end": ..}`，两头按同一个 [`WithOffsetParser`] 读写。
/// 写出时同 `format_ext`（UTC，`+00:00`），读入时字符串里是什么 offset 都行，换算成 UTC。
/// 按系统时区写出用 `utc_range_local_serde!`（feature `datetime-iana`）
///
/// ```ignore
/// hygiea::utc_range_serde!(utc_range_3f, hygiea::datetime::WithOffsetParser::YmdHMS3F);
///
/// #[derive(Serialize, Deserialize)]
/// struct Task {
///     #[serde(with = "utc_range_3f")]
///     window: Range<UtcDateTime>,
/// }
/// ```
#[macro_export]
macro_rules! utc_range_serde {
    ($vis:vis $name:ident, $parser:expr) => {
        $vis mod $name {
            #[allow(unused_imports)]
            use super::*;

            pub fn serialize<S>(
                range: &::std::ops::Range<$crate::datetime::UtcDateTime>,
                serializer: S,
            ) -> ::std::result::Result<S::Ok, S::Error>
            where
                S: $crate::datetime::__serde::Serializer,
            {
                $crate::datetime::__datetime_range::serialize_utc(range, $parser, serializer)
            }

            pub fn deserialize<'de, D>(
                deserializer: D,
            ) -> ::std::result::Result<::std::ops::Range<$crate::datetime::UtcDateTime>, D::Error>
            where
                D: $crate::datetime::__serde::Deserializer<'de>,
            {
                $crate::datetime::__datetime_range::deserialize_utc(deserializer, $parser)
            }
        }
    };
}
/// 同 [`utc_range_serde!`]，读写 `Range<OffsetDateTime>`：写出时同 `format_ext`（值自身的 offset），读入时保留字符串里的 offset。
/// 按系统时区读写用 `offset_range_local_serde!`（feature `datetime-iana`）
#[macro_export]
macro_rules! offset_range_serde {
    ($vis:vis $name:ident, $parser:expr) => {
        $vis mod $name {
            #[allow(unused_imports)]
            use super::*;

            pub fn serialize<S>(
                range: &::std::ops::Range<$crate::datetime::OffsetDateTime>,
                serializer: S,
            ) -> ::std::result::Result<S::Ok, S::Error>
            where
                S: $crate::datetime::__serde::Serializer,
            {
                $crate::datetime::__datetime_range::serialize_offset(range, $parser, serializer)
            }

            pub fn deserialize<'de, D>(
                deserializer: D,
            ) -> ::std::result::Result<::std::ops::Range<$crate::datetime::OffsetDateTime>, D::Error>
            where
                D: $crate::datetime::__serde::Deserializer<'de>,
            {
                $crate::datetime::__datetime_range::deserialize_offset(deserializer, $parser)
            }
        }
    };
}

/// [`utc_range_serde!`]、[`offset_range_serde!`] 生成的模块调用的实现
#[doc(hidden)]
pub mod __datetime_range {
    use std::ops::Range;

    use serde::de::Error as _;
    use serde::ser::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use time::{OffsetDateTime, UtcDateTime};

    use super::{HygieaDateTimeExt, WithOffsetParser};
    use crate::{BaseErr, err};

    pub fn serialize_utc<S: Serializer>(
        range: &Range<UtcDateTime>,
        parser: WithOffsetParser,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let start = range.start.format_ext(parser).map_err(S::Error::custom)?;
        let end = range.end.format_ext(parser).map_err(S::Error::custom)?;
        (start..end).serialize(serializer)
    }

    pub fn deserialize_utc<'de, D: Deserializer<'de>>(
        deserializer: D,
        parser: WithOffsetParser,
    ) -> Result<Range<UtcDateTime>, D::Error> {
        let range = deserialize_offset(deserializer, parser)?;
        Ok(range.start.to_utc()..range.end.to_utc())
    }

    pub fn serialize_offset<S: Serializer>(
        range: &Range<OffsetDateTime>,
        parser: WithOffsetParser,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let start = range.start.format_ext(parser).map_err(S::Error::custom)?;
        let end = range.end.format_ext(parser).map_err(S::Error::custom)?;
        (start..end).serialize(serializer)
    }

    pub fn deserialize_offset<'de, D: Deserializer<'de>>(
        deserializer: D,
        parser: WithOffsetParser,
    ) -> Result<Range<OffsetDateTime>, D::Error> {
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
    use time::macros::{datetime, offset, utc_datetime};
    use time::{PrimitiveDateTime, UtcOffset};

    use super::*;
    use crate::datetime::*;

    // ---- fixtures --------------------------------------------------------

    fn fixture_utc() -> UtcDateTime {
        utc_datetime!(2024-01-05 13:45:06.789_123_456)
    }

    /// 与 [`fixture_utc`] 同一时刻，固定 +08:00 offset。
    fn fixture_offset() -> OffsetDateTime {
        OffsetDateTime::from(fixture_utc()).to_offset(offset!(+8))
    }

    // ---- Formatter / Parser ----------------------------------------------

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

    /// `WithoutOffsetFormatter` 里 `Y` / `HMS` / `Ymd` / `Ymdnosep`、`WithOffsetFormatter` 里 `YmdTHMS3F`
    /// 只能格式化，没有对应的 Parser 变体；用穷尽 match 固化下来，新增变体时这里也要跟着改
    #[test]
    fn test_formatter_only_variants_have_no_parser() {
        {
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
        }

        use WithOffsetFormatter::*;

        let has_parser = |formatter: WithOffsetFormatter| {
            [
                WithOffsetParser::YmdHMS,
                WithOffsetParser::YmdHMS3F,
                WithOffsetParser::YmdHMSnosep,
            ]
            .into_iter()
            .any(|parser| WithOffsetFormatter::from(parser) == formatter)
        };

        for formatter in [YmdHMS, YmdHMS3F, YmdHMSnosep, YmdTHMS3F] {
            let expect_parser = matches!(formatter, YmdHMS | YmdHMS3F | YmdHMSnosep);
            assert_eq!(has_parser(formatter), expect_parser, "{formatter:?}");
        }
    }

    // ---- DateTimeFormatter -----------------------------------------------

    #[test]
    fn test_datetime_formatter_from_str() {
        use WithOffsetFormatter as O;
        use WithoutOffsetFormatter as W;

        let cases = [
            (
                "WithOffset.YmdHMS",
                DateTimeFormatter::WithOffset(O::YmdHMS),
            ),
            (
                "WithOffset.YmdHMS3F",
                DateTimeFormatter::WithOffset(O::YmdHMS3F),
            ),
            (
                "WithOffset.YmdHMSnosep",
                DateTimeFormatter::WithOffset(O::YmdHMSnosep),
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
        assert_eq!(
            WithOffsetFormatter::from(WithOffsetParser::YmdHMS),
            WithOffsetFormatter::YmdHMS
        );
        assert_eq!(
            DateTimeFormatter::from(WithOffsetParser::YmdHMSnosep),
            DateTimeFormatter::WithOffset(WithOffsetFormatter::YmdHMSnosep)
        );
    }

    #[cfg(feature = "log")]
    #[test]
    fn test_datetime_formatter_deserialize_invalid_string_fails() {
        let err = serde_json::from_str::<DateTimeFormatter>("\"bogus\"").unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }

    // ---- HygieaDateTimeExt -----------------------------------------------

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
            (WithOffsetFormatter::YmdHMS, "2024-01-05 13:45:06 +00:00"),
            (
                WithOffsetFormatter::YmdHMS3F,
                "2024-01-05 13:45:06.789 +00:00",
            ),
            (WithOffsetFormatter::YmdHMSnosep, "20240105134506+0000"),
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

    /// 负时间戳（1970 年之前），以及带小数部分的负毫秒
    #[test]
    fn test_from_unix_timestamps_before_epoch() {
        let dt = UtcDateTime::from_secs(-3600).unwrap();
        assert_eq!(dt, utc_datetime!(1969-12-31 23:00:00));

        let dt = UtcDateTime::from_millis(-500).unwrap();
        assert_eq!(dt, utc_datetime!(1969-12-31 23:59:59.500_000_000));

        let dt = UtcDateTime::from_millis(-1_500).unwrap();
        assert_eq!(dt, utc_datetime!(1969-12-31 23:59:58.500_000_000));

        let dt = OffsetDateTime::from_secs(-3600).unwrap();
        assert_eq!(dt, OffsetDateTime::from(utc_datetime!(1969-12-31 23:00:00)));
        let dt = OffsetDateTime::from_millis(-1_500).unwrap();
        assert_eq!(
            dt,
            OffsetDateTime::from(utc_datetime!(1969-12-31 23:59:58.500_000_000))
        );
    }

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

    #[test]
    fn test_shift_cross_month_and_year() {
        let dt = utc_datetime!(2024-01-31 12:00:00);
        assert_eq!(
            dt.shift(Duration::days(1)).unwrap(),
            utc_datetime!(2024-02-01 12:00:00)
        );
        let dt = utc_datetime!(2024-12-31 12:00:00);
        assert_eq!(
            dt.shift(Duration::days(1)).unwrap(),
            utc_datetime!(2025-01-01 12:00:00)
        );
    }

    // ---- HygieaUtcDateTimeExt --------------------------------------------

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

    /// 跨年周：一周的起止落在不同年份
    #[test]
    fn test_utc_week_boundaries_cross_year() {
        // 2024-12-31 是周二，所在周从 2024-12-30 开始，一路跨到 2025-01-05 结束
        let dt = utc_datetime!(2024-12-31 10:00:00);
        assert_eq!(
            dt.start_of_week().unwrap(),
            utc_datetime!(2024-12-30 0:00:00)
        );
        assert_eq!(
            dt.end_of_week().unwrap(),
            utc_datetime!(2025-01-05 23:59:59.999_999_999)
        );

        // 2020-12-31 是周四，所在周结束于 2021-01-03，跨年
        let dt = utc_datetime!(2020-12-31 10:00:00);
        assert_eq!(
            dt.start_of_week().unwrap(),
            utc_datetime!(2020-12-28 0:00:00)
        );
        assert_eq!(
            dt.end_of_week().unwrap(),
            utc_datetime!(2021-01-03 23:59:59.999_999_999)
        );
    }

    /// 世纪年不是闰年：2100 能被 100 整除但不能被 400 整除，2 月只有 28 天
    #[test]
    fn test_utc_end_of_month_century_non_leap_year() {
        let dt = utc_datetime!(2024-02-10 0:00:00);
        assert_eq!(
            dt.end_of_month().unwrap(),
            utc_datetime!(2024-02-29 23:59:59.999_999_999)
        );
        let dt = utc_datetime!(2100-02-10 0:00:00);
        assert_eq!(
            dt.end_of_month().unwrap(),
            utc_datetime!(2100-02-28 23:59:59.999_999_999)
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

    // ---- serde -----------------------------------------------------------

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Rfc3339Fields {
        #[serde(with = "rfc3339_utc")]
        at: UtcDateTime,
        #[serde(default, with = "rfc3339_utc::option")]
        maybe: Option<UtcDateTime>,
    }

    #[test]
    fn rfc3339_utc_reads_z_and_offset_as_utc() {
        let fields: Rfc3339Fields = serde_json::from_str(
            r#"{"at":"2026-10-05T12:57:24.719Z","maybe":"2026-10-05T20:57:24.719+08:00"}"#,
        )
        .unwrap();
        let expected = time::macros::utc_datetime!(2026-10-05 12:57:24.719);
        assert_eq!(fields.at, expected);
        assert_eq!(fields.maybe, Some(expected));
    }

    #[test]
    fn rfc3339_utc_writes_z() {
        let fields = Rfc3339Fields {
            at: time::macros::utc_datetime!(2026-10-05 12:57:24.719),
            maybe: None,
        };
        assert_eq!(
            serde_json::to_string(&fields).unwrap(),
            r#"{"at":"2026-10-05T12:57:24.719Z","maybe":null}"#
        );
    }

    #[test]
    fn rfc3339_utc_option_missing_is_none() {
        let fields: Rfc3339Fields =
            serde_json::from_str(r#"{"at":"2026-10-05T12:57:24Z"}"#).unwrap();
        assert_eq!(fields.maybe, None);
    }

    #[test]
    fn rfc3339_utc_option_some_writes_z() {
        let at = time::macros::utc_datetime!(2026-10-05 12:57:24);
        let fields = Rfc3339Fields {
            at,
            maybe: Some(at),
        };
        assert_eq!(
            serde_json::to_string(&fields).unwrap(),
            r#"{"at":"2026-10-05T12:57:24Z","maybe":"2026-10-05T12:57:24Z"}"#
        );
    }

    #[test]
    fn rfc3339_utc_rejects_missing_offset() {
        // 没有 offset 不知道是哪个时区的时刻，不能当成 UTC 读
        for at in [r#""2026-10-05T12:57:24""#, r#""2026-10-05 12:57:24.719""#] {
            let json = format!(r#"{{"at":{at}}}"#);
            assert!(
                serde_json::from_str::<Rfc3339Fields>(&json).is_err(),
                "{json}"
            );
        }
    }

    // ---- serde：Range<UtcDateTime> / Range<OffsetDateTime> -----------------

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct RangeFields {
        #[serde(with = "ymdhms_utc_range")]
        utc: std::ops::Range<UtcDateTime>,
        #[serde(with = "ymdhms_offset_range")]
        offset: std::ops::Range<OffsetDateTime>,
    }

    #[test]
    fn datetime_range_writes_own_offset() {
        let start = utc_datetime!(2026-10-17 02:00:00);
        let end = utc_datetime!(2026-10-17 06:00:00);
        let fields = RangeFields {
            utc: start..end,
            offset: OffsetDateTime::from(start).to_offset(offset!(+9))
                ..OffsetDateTime::from(end).to_offset(offset!(+9)),
        };
        let json = serde_json::to_string(&fields).unwrap();
        assert_eq!(
            json,
            r#"{"utc":{"start":"2026-10-17 02:00:00 +00:00","end":"2026-10-17 06:00:00 +00:00"},"offset":{"start":"2026-10-17 11:00:00 +09:00","end":"2026-10-17 15:00:00 +09:00"}}"#
        );
        // 原样往返，offset 也不变
        let read = serde_json::from_str::<RangeFields>(&json).unwrap();
        assert_eq!(read, fields);
        assert_eq!(read.offset.start.offset(), offset!(+9));
    }

    #[test]
    fn datetime_range_reads_any_offset() {
        let range = r#"{"start":"2026-10-17 11:00:00 +09:00","end":"2026-10-17 06:00:00 +00:00"}"#;
        let fields: RangeFields =
            serde_json::from_str(&format!(r#"{{"utc":{range},"offset":{range}}}"#)).unwrap();
        let start = utc_datetime!(2026-10-17 02:00:00);
        let end = utc_datetime!(2026-10-17 06:00:00);
        assert_eq!(fields.utc, start..end);
        // 时刻相同，保留字符串里的 offset
        assert_eq!(fields.offset.start, OffsetDateTime::from(start));
        assert_eq!(fields.offset.start.offset(), offset!(+9));
        assert_eq!(fields.offset.end.offset(), offset!(+0));
    }

    #[test]
    fn datetime_range_rejects_missing_offset() {
        // 不带 offset 不知道是哪个时区的时刻，不读
        let range = r#"{"start":"2026-10-17 10:00:00","end":"2026-10-17 14:00:00"}"#;
        let json = format!(r#"{{"utc":{range},"offset":{range}}}"#);
        assert!(serde_json::from_str::<RangeFields>(&json).is_err());
    }

    #[test]
    fn datetime_range_rejects_start_after_end() {
        // 按时刻比：+09:00 的 11:00 是 UTC 02:00，比 UTC 01:00 晚
        let range = r#"{"start":"2026-10-17 11:00:00 +09:00","end":"2026-10-17 01:00:00 +00:00"}"#;
        let json = format!(r#"{{"utc":{range},"offset":{range}}}"#);
        let error = serde_json::from_str::<RangeFields>(&json).unwrap_err();
        assert!(
            error.to_string().contains("range start is after end"),
            "{error}"
        );
    }
}
