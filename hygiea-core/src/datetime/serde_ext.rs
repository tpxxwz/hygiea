//! serde 读写模块：字段上 `#[serde(with = "..")]` 用，把日期时间、时长读写成配置文件、接口里常见的格式。
//! 都从 `hygiea::datetime` 导出，`Option` 字段用模块下的 `option` 子模块，字段可能缺失时再加 `#[serde(default)]`。
//!
//! # `UtcDateTime`：RFC 3339
//!
//! 外部接口常用 RFC 3339（`2026-10-05T12:57:24.719Z`），time 给 `UtcDateTime` 默认的 serde 格式却是
//! `2026-10-05 12:57:24.719`，读不了。字段上用 [`rfc3339_utc`]：解析时带 `Z` 或任意 offset 都换算成 UTC，输出带 `Z`：
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
//! # 时间段 `Range<UtcDateTime>` / `Range<OffsetDateTime>`
//!
//! 存成 `{"start": .., "end": ..}`，两头按同一个 [`WithOffsetParser`] 读写：
//! - 写出同 `format_ext`：`UtcDateTime` 是 `+00:00`，`OffsetDateTime` 是值自身的 offset；
//! - 读入时字符串里必须带 offset，什么 offset 都行：`UtcDateTime` 换算成 UTC，`OffsetDateTime` 保留字符串里的 offset；
//! - 预先生成了 `YmdHMS` 的 [`ymdhms_utc_range`]、[`ymdhms_offset_range`]，别的格式用
//!   [`utc_range_serde!`](crate::utc_range_serde) / [`offset_range_serde!`](crate::offset_range_serde) 生成；
//! - 读入时 `start` 比 `end` 晚报错（按时刻比，和 offset 无关），`start == end` 是空区间。
//!
//! 按系统时区写出（`2026-10-17 10:00:00 +08:00`）用 `_local` 的版本（feature `datetime-iana`）：`ymdhms_utc_range_local`、
//! `ymdhms_offset_range_local`，别的格式用 `utc_range_local_serde!` / `offset_range_local_serde!` 生成。
//! 写出同 `format_ext_local`，换算到系统时区；`Range<OffsetDateTime>` 读入后也换算到系统时区，`Range<UtcDateTime>` 读入换算成 UTC。
//!
//! # `Time`：钟面时间
//!
//! `HH:MM:SS` 用 [`hms`]。时间段 `Range<Time>` 存成 `{"start": .., "end": ..}`，两头按同一个 [`TimeParser`] 读写：
//! `HMS` 用预先生成的 [`hms_range`]，别的格式用 [`time_range_serde!`](crate::time_range_serde) 生成。
//! 和标准库的 `Range` 一样左闭右开，不跨天：读入时 `start` 比 `end` 晚报错（比如想表示跨天的 `22:00..06:00`），
//! `start == end` 是空区间。
//!
//! ```ignore
//! #[derive(Serialize, Deserialize)]
//! struct Task {
//!     #[serde(with = "hygiea::datetime::hms")]
//!     start_time: Time,
//!     #[serde(with = "hygiea::datetime::hms_range")]
//!     every_day_range: Range<Time>,
//! }
//! ```
//!
//! # 时长 `std::time::Duration`
//!
//! 字段类型是标准库的 `std::time::Duration`（等多久、超时多久，不会是负数），不是日期运算用的 `time::Duration`。
//! 三种写法，按字段选一个：
//!
//! | 模块 | 配置里写 | 写出 |
//! |---|---|---|
//! | [`duration`] | 带单位的字符串：`"500ms"`、`"30s"`、`"1h 30m"`、`"2days"`，交给 humantime 解析；数字读不了 | `"1m 30s"` 这样的组合 |
//! | [`duration_secs`] | 整数秒：`30` | 整数秒，不足 1 秒的部分舍掉 |
//! | [`duration_millis`] | 整数毫秒：`500` | 整数毫秒，不足 1 毫秒的部分舍掉 |
//! | [`duration_mins`] | 整数分钟：`60` | 整数分钟，不足 1 分钟的部分舍掉 |
//!
//! 用数字的几个，单位要写在字段名里（`acquire_timeout_secs`、`busy_timeout_millis`），不然看配置不知道 `30` 是什么单位：
//!
//! ```ignore
//! #[derive(Serialize, Deserialize)]
//! struct Config {
//!     #[serde(with = "hygiea::datetime::duration")]
//!     base_timeout: Duration,
//!     #[serde(default, with = "hygiea::datetime::duration_secs::option")]
//!     acquire_timeout_secs: Option<Duration>,
//! }
//! ```

use time::format_description::well_known::Rfc3339;

// ---- UtcDateTime / OffsetDateTime ----------------------------------------

// serde 用的 RFC 3339 读写模块 `rfc3339_utc` 和 `rfc3339_utc::option`，说明见模块文档
time::serde::format_description!(pub rfc3339_utc, UtcDateTime, Rfc3339);

// serde 用的 `Range<UtcDateTime>` 读写模块 `ymdhms_utc_range`，两头都按 `WithOffsetParser::YmdHMS`：
// 写出 UTC（`+00:00`），读入任意 offset 换算成 UTC
crate::utc_range_serde!(pub ymdhms_utc_range, crate::datetime::WithOffsetParser::YmdHMS);

// 同 `ymdhms_utc_range`，读写 `Range<OffsetDateTime>`：按值自身的 offset 写出，读入保留字符串里的 offset
crate::offset_range_serde!(pub ymdhms_offset_range, crate::datetime::WithOffsetParser::YmdHMS);
/// 生成 `Range<UtcDateTime>` 的 serde 读写模块：存成 `{"start": .., "end": ..}`，两头按同一个 [`WithOffsetParser`](crate::datetime::WithOffsetParser) 读写。
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

    use crate::datetime::{HygieaDateTimeExt, WithOffsetParser};
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

// ---- Time --------------------------------------------------------------

// serde 用的 `HH:MM:SS` 读写模块 `hms` 和 `hms::option`，格式同 `TimeParser::HMS`
time::serde::format_description!(pub hms, Time, "[hour]:[minute]:[second]");

// serde 用的 `Range<Time>` 读写模块 `hms_range`，两头都按 `TimeParser::HMS`：`{"start":"02:00:00","end":"06:00:00"}`
crate::time_range_serde!(pub hms_range, crate::datetime::TimeParser::HMS);

/// 生成 `Range<Time>` 的 serde 读写模块：存成 `{"start": .., "end": ..}`，两头按同一个 [`TimeParser`](crate::datetime::TimeParser) 读写
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

    use crate::datetime::{HygieaTimeExt, TimeParser};
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

// ---- 按系统时区读写的时间段（feature `datetime-iana`） -----------------------

#[cfg(feature = "datetime-iana")]
pub use range_local::*;

#[cfg(feature = "datetime-iana")]
mod range_local {
    // serde 用的 `Range<UtcDateTime>` 读写模块 `ymdhms_utc_range_local`，两头都按 `WithOffsetParser::YmdHMS`：
    // 写出系统时区的钟面时间和 offset（`2026-10-17 10:00:00 +08:00`），读入任意 offset 换算成 UTC
    crate::utc_range_local_serde!(pub ymdhms_utc_range_local, crate::datetime::WithOffsetParser::YmdHMS);

    // 同 `ymdhms_utc_range_local`，读写 `Range<OffsetDateTime>`：读入后也换算到系统时区
    crate::offset_range_local_serde!(pub ymdhms_offset_range_local, crate::datetime::WithOffsetParser::YmdHMS);
    /// 同 [`utc_range_serde!`](crate::utc_range_serde)，写出时同 `format_ext_local`：换算到系统时区，带系统时区的 offset。读入时一样，任意 offset 换算成 UTC
    ///
    /// ```ignore
    /// hygiea::utc_range_local_serde!(utc_range_3f, hygiea::datetime::WithOffsetParser::YmdHMS3F);
    /// ```
    #[macro_export]
    macro_rules! utc_range_local_serde {
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
                    $crate::datetime::__datetime_range_local::serialize_utc(range, $parser, serializer)
                }

                pub fn deserialize<'de, D>(
                    deserializer: D,
                ) -> ::std::result::Result<::std::ops::Range<$crate::datetime::UtcDateTime>, D::Error>
                where
                    D: $crate::datetime::__serde::Deserializer<'de>,
                {
                    $crate::datetime::__datetime_range_local::deserialize_utc(deserializer, $parser)
                }
            }
        };
    }
    /// 同 [`offset_range_serde!`](crate::offset_range_serde)，按系统时区读写：写出时同 `format_ext_local`，读入后换算到系统时区
    #[macro_export]
    macro_rules! offset_range_local_serde {
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
                    $crate::datetime::__datetime_range_local::serialize_offset(range, $parser, serializer)
                }

                pub fn deserialize<'de, D>(
                    deserializer: D,
                ) -> ::std::result::Result<::std::ops::Range<$crate::datetime::OffsetDateTime>, D::Error>
                where
                    D: $crate::datetime::__serde::Deserializer<'de>,
                {
                    $crate::datetime::__datetime_range_local::deserialize_offset(deserializer, $parser)
                }
            }
        };
    }

    /// [`utc_range_local_serde!`]、[`offset_range_local_serde!`] 生成的模块调用的实现
    #[doc(hidden)]
    pub mod __datetime_range_local {
        use std::ops::Range;

        use serde::de::Error as _;
        use serde::ser::Error as _;
        use serde::{Deserializer, Serialize, Serializer};
        use time::{OffsetDateTime, UtcDateTime};

        use crate::datetime::local::to_local;
        use crate::datetime::{__datetime_range, HygieaOffsetDateTimeExt, WithOffsetParser};

        pub fn serialize_utc<S: Serializer>(
            range: &Range<UtcDateTime>,
            parser: WithOffsetParser,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            let range = OffsetDateTime::from(range.start)..OffsetDateTime::from(range.end);
            serialize_offset(&range, parser, serializer)
        }

        pub fn deserialize_utc<'de, D: Deserializer<'de>>(
            deserializer: D,
            parser: WithOffsetParser,
        ) -> Result<Range<UtcDateTime>, D::Error> {
            __datetime_range::deserialize_utc(deserializer, parser)
        }

        pub fn serialize_offset<S: Serializer>(
            range: &Range<OffsetDateTime>,
            parser: WithOffsetParser,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            let start = range
                .start
                .format_ext_local(parser.into())
                .map_err(S::Error::custom)?;
            let end = range
                .end
                .format_ext_local(parser.into())
                .map_err(S::Error::custom)?;
            (start..end).serialize(serializer)
        }

        pub fn deserialize_offset<'de, D: Deserializer<'de>>(
            deserializer: D,
            parser: WithOffsetParser,
        ) -> Result<Range<OffsetDateTime>, D::Error> {
            let range = __datetime_range::deserialize_offset(deserializer, parser)?;
            let start = to_local(&range.start).map_err(D::Error::custom)?;
            let end = to_local(&range.end).map_err(D::Error::custom)?;
            Ok(start..end)
        }
    }
}

// ---- std::time::Duration ------------------------------------------------

/// `std::time::Duration` 读写成带单位的字符串（`"500ms"`、`"30s"`、`"1h 30m"`），交给 humantime 解析和输出。
/// `Option<Duration>` 用 [`duration::option`](self::duration::option)
pub mod duration {
    use std::time::Duration;

    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        humantime_serde::serialize(value, serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        humantime_serde::deserialize(deserializer)
    }

    /// 同 [`duration`](super::duration)，读写 `Option<Duration>`：`None` 写出 `null`
    pub mod option {
        use std::time::Duration;

        use serde::{Deserializer, Serializer};

        pub fn serialize<S: Serializer>(
            value: &Option<Duration>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            humantime_serde::option::serialize(value, serializer)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Duration>, D::Error> {
            humantime_serde::option::deserialize(deserializer)
        }
    }
}

/// `std::time::Duration` 读写成整数秒：读入 `30` 是 30 秒，写出时不足 1 秒的部分舍掉。
/// `Option<Duration>` 用 [`duration_secs::option`](self::duration_secs::option)
pub mod duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        value.as_secs().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        u64::deserialize(deserializer).map(Duration::from_secs)
    }

    /// 同 [`duration_secs`](super::duration_secs)，读写 `Option<Duration>`：`None` 写出 `null`
    pub mod option {
        use std::time::Duration;

        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        pub fn serialize<S: Serializer>(
            value: &Option<Duration>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            value.map(|value| value.as_secs()).serialize(serializer)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Duration>, D::Error> {
            Option::<u64>::deserialize(deserializer).map(|value| value.map(Duration::from_secs))
        }
    }
}

/// `std::time::Duration` 读写成整数毫秒：读入 `500` 是 500 毫秒，写出时不足 1 毫秒的部分舍掉，超出 `u64` 的报错。
/// `Option<Duration>` 用 [`duration_millis::option`](self::duration_millis::option)
pub mod duration_millis {
    use std::time::Duration;

    use serde::ser::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        to_millis(value)
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        u64::deserialize(deserializer).map(Duration::from_millis)
    }

    /// `as_millis` 是 `u128`，配置里按 `u64` 存
    fn to_millis(value: &Duration) -> Result<u64, String> {
        u64::try_from(value.as_millis())
            .map_err(|_| format!("duration too large for millis: {value:?}"))
    }

    /// 同 [`duration_millis`](super::duration_millis)，读写 `Option<Duration>`：`None` 写出 `null`
    pub mod option {
        use std::time::Duration;

        use serde::ser::Error as _;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        pub fn serialize<S: Serializer>(
            value: &Option<Duration>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            value
                .as_ref()
                .map(super::to_millis)
                .transpose()
                .map_err(S::Error::custom)?
                .serialize(serializer)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Duration>, D::Error> {
            Option::<u64>::deserialize(deserializer).map(|value| value.map(Duration::from_millis))
        }
    }
}

/// `std::time::Duration` 读写成整数分钟：读入 `60` 是 60 分钟，写出时不足 1 分钟的部分舍掉。
/// `Option<Duration>` 用 [`duration_mins::option`](self::duration_mins::option)
pub mod duration_mins {
    use std::time::Duration;

    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    const SECS_PER_MIN: u64 = 60;

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        (value.as_secs() / SECS_PER_MIN).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        from_mins(u64::deserialize(deserializer)?).map_err(D::Error::custom)
    }

    /// 分钟换成秒会溢出的报错
    fn from_mins(mins: u64) -> Result<Duration, String> {
        mins.checked_mul(SECS_PER_MIN)
            .map(Duration::from_secs)
            .ok_or_else(|| format!("duration too large: {mins} mins"))
    }

    /// 同 [`duration_mins`](super::duration_mins)，读写 `Option<Duration>`：`None` 写出 `null`
    pub mod option {
        use std::time::Duration;

        use serde::de::Error as _;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        use super::SECS_PER_MIN;

        pub fn serialize<S: Serializer>(
            value: &Option<Duration>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            value
                .map(|value| value.as_secs() / SECS_PER_MIN)
                .serialize(serializer)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Duration>, D::Error> {
            Option::<u64>::deserialize(deserializer)?
                .map(super::from_mins)
                .transpose()
                .map_err(D::Error::custom)
        }
    }
}

#[cfg(test)]
mod tests {
    mod datetime_range {
        use time::macros::{offset, utc_datetime};
        use time::{OffsetDateTime, UtcDateTime};

        use crate::datetime::*;

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
            let range =
                r#"{"start":"2026-10-17 11:00:00 +09:00","end":"2026-10-17 06:00:00 +00:00"}"#;
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
            let range =
                r#"{"start":"2026-10-17 11:00:00 +09:00","end":"2026-10-17 01:00:00 +00:00"}"#;
            let json = format!(r#"{{"utc":{range},"offset":{range}}}"#);
            let error = serde_json::from_str::<RangeFields>(&json).unwrap_err();
            assert!(
                error.to_string().contains("range start is after end"),
                "{error}"
            );
        }
    }

    mod time_range {
        use std::ops::Range;

        use time::Time;

        use crate::datetime::*;

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
            let json = r#"{"hms":{"start":"02:00:00","end":"02:00:00"},"hm":{"start":"08:00","end":"08:00"}}"#;
            let fields = serde_json::from_str::<RangeFields>(json).unwrap();
            assert!(fields.hms.is_empty());
        }
    }

    #[cfg(feature = "datetime-iana")]
    mod datetime_range_local {
        use serial_test::serial;
        use time::macros::offset;
        use time::{OffsetDateTime, UtcDateTime};

        use crate::datetime::local::test_tz::{TzGuard, tz};
        use crate::datetime::*;

        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct RangeFields {
            #[serde(with = "ymdhms_utc_range_local")]
            utc: std::ops::Range<UtcDateTime>,
            #[serde(with = "ymdhms_offset_range_local")]
            offset: std::ops::Range<OffsetDateTime>,
        }

        #[test]
        #[serial]
        fn datetime_range_local_writes_local_offset() {
            let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
            let start = time::macros::utc_datetime!(2026-10-17 02:00:00);
            let end = time::macros::utc_datetime!(2026-10-17 06:00:00);
            let fields = RangeFields {
                utc: start..end,
                // 值自身是 +09:00，写出时也换算到系统时区
                offset: OffsetDateTime::from(start).to_offset(offset!(+9))
                    ..OffsetDateTime::from(end).to_offset(offset!(+9)),
            };
            let json = serde_json::to_string(&fields).unwrap();
            let expected =
                r#"{"start":"2026-10-17 10:00:00 +08:00","end":"2026-10-17 14:00:00 +08:00"}"#;
            assert_eq!(json, format!(r#"{{"utc":{expected},"offset":{expected}}}"#));
        }

        #[test]
        #[serial]
        fn datetime_range_local_reads_any_offset_as_local() {
            let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
            let range =
                r#"{"start":"2026-10-17 11:00:00 +09:00","end":"2026-10-17 06:00:00 +00:00"}"#;
            let fields: RangeFields =
                serde_json::from_str(&format!(r#"{{"utc":{range},"offset":{range}}}"#)).unwrap();
            let start = time::macros::utc_datetime!(2026-10-17 02:00:00);
            let end = time::macros::utc_datetime!(2026-10-17 06:00:00);
            assert_eq!(fields.utc, start..end);
            // 时刻不变，offset 换成系统时区的
            assert_eq!(fields.offset.start, OffsetDateTime::from(start));
            assert_eq!(fields.offset.start.offset(), offset!(+8));
            assert_eq!(fields.offset.end.offset(), offset!(+8));
        }
    }

    mod duration {
        use std::time::Duration;

        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct DurationFields {
            #[serde(with = "crate::datetime::duration")]
            text: Duration,
            #[serde(default, with = "crate::datetime::duration::option")]
            text_opt: Option<Duration>,
            #[serde(with = "crate::datetime::duration_secs")]
            secs: Duration,
            #[serde(default, with = "crate::datetime::duration_secs::option")]
            secs_opt: Option<Duration>,
            #[serde(with = "crate::datetime::duration_millis")]
            millis: Duration,
            #[serde(default, with = "crate::datetime::duration_millis::option")]
            millis_opt: Option<Duration>,
            #[serde(default, with = "crate::datetime::duration_mins")]
            mins: Duration,
            #[serde(default, with = "crate::datetime::duration_mins::option")]
            mins_opt: Option<Duration>,
        }

        #[test]
        fn duration_reads_text_secs_millis() {
            let fields: DurationFields = serde_json::from_str(
                r#"{"text":"1h 30m","text_opt":"500ms","secs":30,"secs_opt":5,"millis":500,"millis_opt":1500,"mins":60,"mins_opt":2}"#,
            )
            .unwrap();
            assert_eq!(
                fields,
                DurationFields {
                    text: Duration::from_secs(90 * 60),
                    text_opt: Some(Duration::from_millis(500)),
                    secs: Duration::from_secs(30),
                    secs_opt: Some(Duration::from_secs(5)),
                    millis: Duration::from_millis(500),
                    millis_opt: Some(Duration::from_millis(1500)),
                    mins: Duration::from_secs(60 * 60),
                    mins_opt: Some(Duration::from_secs(2 * 60)),
                }
            );
        }

        #[test]
        fn duration_writes_text_secs_millis() {
            let fields = DurationFields {
                text: Duration::from_secs(90),
                text_opt: None,
                secs: Duration::from_millis(30_900),
                secs_opt: Some(Duration::from_secs(5)),
                millis: Duration::from_micros(500_900),
                millis_opt: None,
                mins: Duration::from_secs(150),
                mins_opt: Some(Duration::from_secs(60)),
            };
            // 秒、毫秒写出时舍掉不足一个单位的部分
            assert_eq!(
                serde_json::to_string(&fields).unwrap(),
                r#"{"text":"1m 30s","text_opt":null,"secs":30,"secs_opt":5,"millis":500,"millis_opt":null,"mins":2,"mins_opt":1}"#
            );
        }

        #[test]
        fn duration_option_missing_or_null_is_none() {
            let fields: DurationFields =
                serde_json::from_str(r#"{"text":"3s","text_opt":null,"secs":30,"millis":500}"#)
                    .unwrap();
            assert_eq!(fields.text_opt, None);
            assert_eq!(fields.secs_opt, None);
            assert_eq!(fields.millis_opt, None);
            assert_eq!(fields.mins, Duration::ZERO);
            assert_eq!(fields.mins_opt, None);
        }

        #[test]
        fn duration_rejects_wrong_kind() {
            for json in [
                // 带单位的写成了数字、没写单位
                r#"{"text":500,"secs":30,"millis":500}"#,
                r#"{"text":"500","secs":30,"millis":500}"#,
                // 数字的写成了字符串、负数
                r#"{"text":"3s","secs":"30s","millis":500}"#,
                r#"{"text":"3s","secs":30,"millis":-1}"#,
                // 分钟换成秒溢出
                r#"{"text":"3s","secs":30,"millis":500,"mins":18446744073709551615}"#,
            ] {
                assert!(
                    serde_json::from_str::<DurationFields>(json).is_err(),
                    "{json}"
                );
            }
        }
    }
}
