//! 日期时间：基于 [`time`]，补上项目里常用的格式、时间戳、日历边界和系统时区换算。
//!
//! # 两种类型
//!
//! - [`UtcDateTime`]：UTC 时刻。存储、传输、比较、按 UTC 切日都用它。
//!   日历边界见 [`HygieaUtcDateTimeExt`]。
//! - [`OffsetDateTime`]：一个时刻加一个显示用的 offset，offset 保持 time 的原始语义，**不保证是系统时区**：
//!   - [`now()`]、`from_millis`、`from_secs` 得到的是 UTC offset；
//!   - `parse_ext_rfc3339`、`parse_ext_with_offset` 保留输入里的 offset；
//!   - 带 `_local` 的方法得到的才是系统时区的 offset。
//!
//! 两种类型共有的能力（时间戳、RFC 3339、格式化、平移）见 [`HygieaDateTimeExt`]。
//!
//! # 系统时区（feature `datetime-iana`）
//!
//! 按系统时区做的事都在 `HygieaOffsetDateTimeExt` 里，方法名带 `_local`。
//! 这些方法先把输入换算到系统时区再计算，**结果只取决于时刻，和输入带什么 offset 无关**，
//! 所以 `now()` 的结果、解析来的带 offset 的时间都可以直接调用。
//!
//! 系统时区在第一次使用时确定并缓存：`TZ` 环境变量 → 系统设置（unix 上是 `/etc/localtime`）→ UTC。
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
//! # 错误与返回值
//!
//! - 不会失败的方法直接返回值（比如 `UtcDateTime::start_of_day`）；
//! - 可能失败的（越界、解析失败）返回 `Result<_, HyErr>`，错误码是 `BaseErr::DateError`；
//! - 有多种可能结果的（本地钟面时间对应 0/1/2 个时刻）原样返回 `OffsetResult`。
//!
//! # 格式名
//!
//! [`DateTimeFormatter`] 可以从配置里的名字解析（`FromStr` / serde），名字就是 `类别.变体`，
//! 比如 `WithOffset.YmdTHMS3F`、`WithoutOffset.YmdHMS`；[`WithoutOffsetParser`] 也能直接当格式用。

use time::{OffsetDateTime, UtcDateTime};

mod layout;
mod utc;

#[cfg(feature = "datetime-iana")]
pub(super) mod local;

#[cfg(feature = "datetime-chrono")]
mod chrono_bridge;

pub use layout::*;
pub use utc::*;

#[cfg(feature = "datetime-iana")]
pub use local::{HygieaOffsetDateTimeExt, now_local};
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
