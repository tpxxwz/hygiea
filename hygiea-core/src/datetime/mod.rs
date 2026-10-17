//! 日期时间：基于 [`time`]，补上项目里常用的格式、时间戳、日历边界和系统时区换算。
//!
//! # 类型
//!
//! - [`UtcDateTime`]：UTC 时刻。存储、传输、比较、按 UTC 切日都用它。
//!   日历边界见 [`HygieaUtcDateTimeExt`]。
//! - [`OffsetDateTime`]：一个时刻加一个显示用的 offset，offset 保持 time 的原始语义，**不保证是系统时区**：
//!   - [`now()`]、`from_millis`、`from_secs` 得到的是 UTC offset；
//!   - `parse_ext_rfc3339`、`parse_ext_with_offset` 保留输入里的 offset；
//!   - 带 `_local` 的方法得到的才是系统时区的 offset。
//! - [`Time`]：钟面时间，不带日期和 offset，是哪个时区的由取它的地方决定：[`now_time()`] 是 UTC 的，
//!   `now_time_local()` 是系统时区的。格式和解析见 [`HygieaTimeExt`]。
//!
//! `UtcDateTime`、`OffsetDateTime` 共有的能力（时间戳、RFC 3339、格式化、平移）见 [`HygieaDateTimeExt`]。
//!
//! # 默认和 `_local`
//!
//! 默认的版本只按值自身的 offset（`UtcDateTime` 就是 UTC）算，不依赖系统时区；按系统时区算的是另一份，
//! 名字带 `_local`（方法、serde 模块、宏都是），在 feature `datetime-iana` 下。比如 `format_ext` 和 `format_ext_local`、
//! [`ymdhms_utc_range`] 和 `ymdhms_utc_range_local`。
//!
//! 不带 offset 的字符串只按系统时区解释（`parse_ext_local`），不会当成 UTC 读：没有 offset 不知道是哪个时区的时刻。
//!
//! # 错误与返回值
//!
//! - 不会失败的方法直接返回值（比如 `UtcDateTime::start_of_day`）；
//! - 可能失败的（越界、解析失败）返回 [`Result<_>`](crate::Result)（错误是 `HyErr`），错误码是 `BaseErr::DateError`；
//! - 有多种可能结果的（本地钟面时间对应 0/1/2 个时刻）原样返回 `OffsetResult`。
//!
//! # 文件
//!
//! - `datetime_ext.rs`：`UtcDateTime` / `OffsetDateTime` 的 Formatter / Parser 和格式名、[`HygieaDateTimeExt`]、
//!   [`HygieaUtcDateTimeExt`]；serde 读写模块 [`rfc3339_utc`]、`Range` 的 [`ymdhms_utc_range`] / [`ymdhms_offset_range`]
//! - `local.rs`：系统时区、`now_local`、`WithoutOffsetParser` 的解析、`HygieaOffsetDateTimeExt`、
//!   `Range` 的 `_local` serde 读写模块（feature `datetime-iana`）
//! - `time_ext.rs`：[`Time`] 的 [`TimeFormatter`] / [`TimeParser`]、[`HygieaTimeExt`]；serde 读写模块 [`hms`]、`Range` 的 [`hms_range`]
//! - `chrono_bridge.rs`：和 chrono 互转（feature `datetime-chrono`）

/// 公开接口里用到了 time 的类型，再导出 time，下游不用自己加依赖，也不会和这里的版本对不上。
/// 主类型直接放在这里：`hygiea::datetime::UtcDateTime`，其余经 `hygiea::datetime::time::..`
pub use time;

/// 宏生成的 serde 模块用
#[doc(hidden)]
pub use serde as __serde;
pub use time::{OffsetDateTime, Time, UtcDateTime};

mod datetime_ext;
mod time_ext;

#[cfg(feature = "datetime-iana")]
pub(super) mod local;

#[cfg(feature = "datetime-chrono")]
mod chrono_bridge;

pub use datetime_ext::*;
pub use time_ext::*;

#[cfg(feature = "datetime-iana")]
pub use local::{
    __datetime_range_local, HygieaOffsetDateTimeExt, now_local, ymdhms_offset_range_local,
    ymdhms_utc_range_local,
};

/// `parse_ext_local` 的返回类型（钟面时间可能唯一、歧义或不存在）。再导出后下游不用自己加
/// time-tz 依赖去 match 它，也不会撞上版本对不上
#[cfg(feature = "datetime-iana")]
pub use time_tz::OffsetResult;

#[cfg(feature = "datetime-chrono")]
pub use chrono_bridge::*;

/// 当前 UTC 时刻。
pub fn now_utc() -> UtcDateTime {
    UtcDateTime::now()
}

/// 当前 UTC 时刻的 `OffsetDateTime` 表示，offset 是 UTC，不是系统时区。
///
/// 要系统时区的当前时间用 `now_local`；按系统时区格式化或切日直接调用带 `_local` 的方法即可。
pub fn now() -> OffsetDateTime {
    now_utc().into()
}

/// 当前 UTC 的钟面时间，取自 [`now_utc`]
pub fn now_time() -> Time {
    now_utc().time()
}

/// 当前系统时区的钟面时间，取自 `now_local`（它也是从 [`now`] 换算来的）
#[cfg(feature = "datetime-iana")]
pub fn now_time_local() -> Time {
    now_local().time()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::UtcOffset;

    // ---- clock ---------------------------------------------------------------

    #[test]
    fn test_now_is_real_time() {
        let real = UtcDateTime::now();
        assert!((now_utc() - real).whole_seconds().abs() < 60);
        assert_eq!(now().offset(), UtcOffset::UTC);
    }
}
