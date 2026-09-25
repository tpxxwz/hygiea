//! 系统本地时区（feature `datetime-iana`）：检测并缓存系统时区，提供 `now_local` 和 `_local` 方法。

use std::sync::OnceLock;

use time::PrimitiveDateTime;
use time::format_description::BorrowedFormatItem;
use time::{Duration, Month, OffsetDateTime, Time};
use time_tz::{Offset, OffsetDateTimeExt, OffsetResult, PrimitiveDateTimeExt, TimeZone, Tz};

use super::*;
use crate::{BaseErr, HyErr, ResultExt, err};

static LOCAL_TIMEZONE: OnceLock<&'static Tz> = OnceLock::new();

#[cfg(test)]
static LOCAL_TIMEZONE_OVERRIDE: parking_lot::RwLock<Option<&'static Tz>> =
    parking_lot::RwLock::new(None);

/// 测试专用：覆盖 `local_timezone()` 返回的时区，`None` 恢复系统时区。
#[cfg(test)]
pub(super) fn set_local_timezone(tz: Option<&'static Tz>) {
    *LOCAL_TIMEZONE_OVERRIDE.write() = tz;
}

pub(crate) fn init() {
    let _ = local_timezone();
}

fn local_timezone() -> &'static Tz {
    #[cfg(test)]
    if let Some(tz) = *LOCAL_TIMEZONE_OVERRIDE.read() {
        return tz;
    }
    LOCAL_TIMEZONE.get_or_init(|| detect_timezone(std::env::var("TZ").ok()))
}

/// 顺序同 C 库的 localtime()：TZ 环境变量 → 系统设置（unix 上是 /etc/localtime 符号链接）→ UTC。
/// TZ 的值交给 time-tz 的 IANA 数据库查，开头的 `:` 先去掉（glibc 的写法，比如 `:Asia/Shanghai`、
/// `:/etc/localtime`）；查不到（拼错、文件路径、POSIX 规则串等）就往下走
fn detect_timezone(tz_env: Option<String>) -> &'static Tz {
    if let Some(tz) = tz_env
        .and_then(|name| time_tz::timezones::get_by_name(name.strip_prefix(':').unwrap_or(&name)))
    {
        return tz;
    }
    time_tz::system::get_timezone().unwrap_or_else(|error| {
        eprintln!(
            "ERROR hygiea: failed to determine local timezone (TZ unset or unknown, system: {error}); falling back to UTC"
        );
        time_tz::timezones::db::UTC
    })
}

/// 换到系统时区在该时刻使用的 offset。time-tz 的 `to_timezone` 在时间贴近上下界（±9999 年）、
/// 加上 offset 越界时会 panic，这里先取 offset，再用 time 的 `checked_to_offset`，越界返回 `DateError`
fn to_local(datetime: &OffsetDateTime) -> Result<OffsetDateTime, HyErr> {
    let offset = local_timezone().get_offset_utc(datetime).to_utc();
    datetime.checked_to_offset(offset).ok_or_else(|| {
        err!(
            BaseErr::DateError,
            format!("convert to local timezone out of range: datetime={datetime}")
        )
    })
}

/// 把不带 offset 的钟面时间（比如 "2024-03-10 02:30"）当成系统时区的本地时间，找出它对应的时刻。
///
/// 钟面时间换成时刻，要看这个时区在那个时候用的是哪个 offset，夏令时切换时不一定只有一个答案：
/// - `Some`：只对应一个时刻，绝大多数情况都是这种；
/// - `None`：这个钟面时间不存在，比如拨快时 02:00 直接跳到 03:00，02:30 就没有；
/// - `Ambiguous`：出现两次，比如回拨时 01:00~02:00 走了两遍，01:30 对应两个时刻。
///
/// 结果原样返回，由调用方决定怎么处理；`parse_ext_local` 和 `*_local` 日历边界都靠它把钟面时间换成时刻
fn resolve_local(datetime: PrimitiveDateTime) -> OffsetResult<OffsetDateTime> {
    datetime.assume_timezone(local_timezone())
}

/// 当前时刻转换到系统 IANA 时区在该时刻使用的本地 offset。
///
/// 当前时间离上下界很远，不会越界，所以不返回 `Result`
pub fn now_local() -> OffsetDateTime {
    now().to_timezone(local_timezone())
}

impl WithoutOffsetParser {
    fn description(self) -> &'static [BorrowedFormatItem<'static>] {
        WithoutOffsetFormatter::from(self).description()
    }

    pub fn parse(self, input: &str) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let datetime = PrimitiveDateTime::parse(input, self.description()).wrap_err(|| {
            err!(
                BaseErr::DateError,
                format!("parse datetime without offset failed, input={input}")
            )
        })?;
        Ok(resolve_local(datetime))
    }
}

/// `OffsetDateTime` 专有的系统 IANA 本地时区扩展。
///
/// 所有 `_local` 方法先把输入换算到系统时区再计算，结果只取决于时刻，和输入带什么 offset 无关。
/// 返回 `OffsetResult` 的方法，夏令时下可能是 `None`（钟面时间不存在）或 `Ambiguous`（出现两次），
/// 由调用方按业务处理，库不替调用方选。
pub trait HygieaOffsetDateTimeExt: HygieaDateTimeExt {
    // ---- 解析与格式化 ------------------------------------------------------

    /// 按携带 offset 的指定格式解析，并保留输入中的 offset。
    fn parse_ext_with_offset(s: &str, parser: WithOffsetParser) -> Result<OffsetDateTime, HyErr>;
    /// 将不携带 offset 的钟面时间按缓存的系统 IANA 时区解析。
    ///
    /// 返回值保留唯一、歧义或不存在三种时区解析结果。
    fn parse_ext_local(
        s: &str,
        parser: WithoutOffsetParser,
    ) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    /// 转换到系统 IANA 时区在该时间点使用的 offset，再按指定格式输出。
    fn format_ext_local(&self, formatter: DateTimeFormatter) -> Result<String, HyErr>;

    // ---- 时间平移 ------------------------------------------------------------

    /// 在绝对时间线上平移指定时长，再转换到系统 IANA 时区。
    fn shift_local(&self, duration: Duration) -> Result<OffsetDateTime, HyErr>;

    // ---- 日边界 ----------------------------------------------------------------

    /// 按系统 IANA 时区返回当前本地日期的开始。
    fn start_of_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    /// 按系统 IANA 时区返回当前本地日期的结束。做区间判断用半开区间 `[start_of_day_local, start_of_next_day_local)`
    fn end_of_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    /// 按系统 IANA 时区返回下一天的开始。
    fn start_of_next_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;

    // ---- 周边界 ----------------------------------------------------------------

    /// 按系统 IANA 时区返回当前本地周的开始，周一为一周第一天。
    fn start_of_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    /// 按系统 IANA 时区返回当前本地周的结束，周日为一周最后一天。
    /// 做区间判断用半开区间 `[start_of_week_local, start_of_next_week_local)`
    fn end_of_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    /// 按系统 IANA 时区返回下一周（下周一）的开始。
    fn start_of_next_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;

    // ---- 月边界 ----------------------------------------------------------------

    /// 按系统 IANA 时区返回当前本地月的开始。
    fn start_of_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    /// 按系统 IANA 时区返回当前本地月的结束。做区间判断用半开区间 `[start_of_month_local, start_of_next_month_local)`
    fn end_of_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    /// 按系统 IANA 时区返回下个月的开始。
    fn start_of_next_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;

    // ---- 年边界 ----------------------------------------------------------------

    /// 按系统 IANA 时区返回当前本地年的开始。
    fn start_of_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    /// 按系统 IANA 时区返回当前本地年的结束。做区间判断用半开区间 `[start_of_year_local, start_of_next_year_local)`
    fn end_of_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
    /// 按系统 IANA 时区返回下一年的开始。
    fn start_of_next_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr>;
}

impl HygieaOffsetDateTimeExt for OffsetDateTime {
    fn parse_ext_with_offset(s: &str, parser: WithOffsetParser) -> Result<OffsetDateTime, HyErr> {
        parser.parse(s)
    }

    fn parse_ext_local(
        s: &str,
        parser: WithoutOffsetParser,
    ) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        parser.parse(s)
    }

    fn format_ext_local(&self, formatter: DateTimeFormatter) -> Result<String, HyErr> {
        HygieaDateTimeExt::format_ext(&to_local(self)?, formatter)
    }

    fn shift_local(&self, duration: Duration) -> Result<OffsetDateTime, HyErr> {
        let shifted = self.checked_add(duration).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("shift datetime out of range: datetime={self}, duration={duration}")
            )
        })?;
        to_local(&shifted)
    }

    fn start_of_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        Ok(resolve_local(PrimitiveDateTime::new(
            local.date(),
            Time::MIDNIGHT,
        )))
    }

    fn end_of_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        Ok(resolve_local(PrimitiveDateTime::new(
            local.date(),
            Time::MAX,
        )))
    }

    fn start_of_next_day_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let date = local.date().next_day().ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("start_of_next_day_local out of range: datetime={self}")
            )
        })?;
        Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
    }

    fn start_of_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let days = local.weekday().number_days_from_monday() as i64;
        let date = local
            .date()
            .checked_sub(Duration::days(days))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("start_of_week_local out of range: datetime={self}")
                )
            })?;
        Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
    }

    fn end_of_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let days = 6 - local.weekday().number_days_from_monday() as i64;
        let date = local
            .date()
            .checked_add(Duration::days(days))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_week_local out of range: datetime={self}")
                )
            })?;
        Ok(resolve_local(PrimitiveDateTime::new(date, Time::MAX)))
    }

    fn start_of_next_week_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let days = 7 - local.weekday().number_days_from_monday() as i64;
        let date = local
            .date()
            .checked_add(Duration::days(days))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("start_of_next_week_local out of range: datetime={self}")
                )
            })?;
        Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
    }

    fn start_of_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let date = local
            .date()
            .replace_day(1)
            .wrap_err(|| err!(BaseErr::DateError, "start_of_month_local failed"))?;
        Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
    }

    fn end_of_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let first = PrimitiveDateTime::new(
            local
                .date()
                .replace_day(1)
                .wrap_err(|| err!(BaseErr::DateError, "end_of_month_local failed"))?,
            Time::MIDNIGHT,
        );
        let next = first
            .checked_add(Duration::days(31))
            .and_then(|datetime| datetime.replace_day(1).ok())
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_month_local out of range: datetime={self}")
                )
            })?;
        let end = next.checked_sub(Duration::nanoseconds(1)).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("end_of_month_local out of range: datetime={self}")
            )
        })?;
        Ok(resolve_local(end))
    }

    fn start_of_next_month_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let first = local
            .date()
            .replace_day(1)
            .wrap_err(|| err!(BaseErr::DateError, "start_of_next_month_local failed"))?;
        let date = first
            .checked_add(Duration::days(31))
            .and_then(|date| date.replace_day(1).ok())
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("start_of_next_month_local out of range: datetime={self}")
                )
            })?;
        Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
    }

    fn start_of_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let date = local
            .date()
            .replace_day(1)
            .and_then(|date| date.replace_month(Month::January))
            .wrap_err(|| err!(BaseErr::DateError, "start_of_year_local failed"))?;
        Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
    }

    fn end_of_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let next_year = local.year().checked_add(1).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("end_of_year_local out of range: datetime={self}")
            )
        })?;
        let date = local
            .date()
            .replace_day(1)
            .and_then(|date| date.replace_month(Month::January))
            .and_then(|date| date.replace_year(next_year))
            .wrap_err(|| err!(BaseErr::DateError, "end_of_year_local failed"))?;
        let end = PrimitiveDateTime::new(date, Time::MIDNIGHT)
            .checked_sub(Duration::nanoseconds(1))
            .ok_or_else(|| {
                err!(
                    BaseErr::DateError,
                    format!("end_of_year_local out of range: datetime={self}")
                )
            })?;
        Ok(resolve_local(end))
    }

    fn start_of_next_year_local(&self) -> Result<OffsetResult<OffsetDateTime>, HyErr> {
        let local = to_local(self)?;
        let next_year = local.year().checked_add(1).ok_or_else(|| {
            err!(
                BaseErr::DateError,
                format!("start_of_next_year_local out of range: datetime={self}")
            )
        })?;
        let date = local
            .date()
            .replace_day(1)
            .and_then(|date| date.replace_month(Month::January))
            .and_then(|date| date.replace_year(next_year))
            .wrap_err(|| err!(BaseErr::DateError, "start_of_next_year_local failed"))?;
        Ok(resolve_local(PrimitiveDateTime::new(date, Time::MIDNIGHT)))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serial_test::serial;
    use time::Date;
    use time::macros::{datetime, offset};

    use super::*;

    // ---- fixtures --------------------------------------------------------

    fn tz(name: &str) -> &'static Tz {
        time_tz::timezones::get_by_name(name).expect("known timezone")
    }

    fn unique(result: Result<OffsetResult<OffsetDateTime>, HyErr>) -> OffsetDateTime {
        match result {
            Ok(OffsetResult::Some(dt)) => dt,
            Ok(other) => panic!("expected unique local datetime, got {other:?}"),
            Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    fn assert_local_eq(actual: OffsetDateTime, expected: OffsetDateTime) {
        assert_eq!(actual, expected, "absolute datetime differs");
        assert_eq!(actual.offset(), expected.offset(), "UTC offset differs");
        assert_eq!(actual.date(), expected.date(), "local date differs");
        assert_eq!(actual.time(), expected.time(), "local time differs");
    }

    struct TzGuard;

    impl TzGuard {
        fn new(tz: &'static Tz) -> Self {
            set_local_timezone(Some(tz));
            Self
        }
    }

    impl Drop for TzGuard {
        fn drop(&mut self) {
            set_local_timezone(None);
        }
    }

    // ---- now_local -------------------------------------------------------

    #[test]
    #[serial]
    fn test_now_local() {
        let tz = tz("Asia/Shanghai");
        let _tz_guard = TzGuard::new(tz);
        let now = now_local();
        assert_eq!(now.offset(), offset!(+8));
        assert!((now - OffsetDateTime::now_utc()).whole_seconds().abs() < 60);
    }

    // ---- parse -----------------------------------------------------------

    #[test]
    fn test_parse_ext_with_offset() {
        let dt = OffsetDateTime::parse_ext_with_offset(
            "2024-01-05 21:45:06 +08:00",
            WithOffsetParser::YmdHMS,
        )
        .unwrap();
        assert_eq!(dt.hour(), 21);
        assert_eq!(dt.offset(), offset!(+8));

        let dt = OffsetDateTime::parse_ext_with_offset(
            "2024-01-05 21:45:06.789 +08:00",
            WithOffsetParser::YmdHMS3F,
        )
        .unwrap();
        assert_eq!(dt.nanosecond(), 789_000_000);

        let nosep = OffsetDateTime::parse_ext_with_offset(
            "20240105214506+0800",
            WithOffsetParser::YmdHMSnosep,
        )
        .unwrap();
        let separated = OffsetDateTime::parse_ext_with_offset(
            "2024-01-05 21:45:06 +08:00",
            WithOffsetParser::YmdHMS,
        )
        .unwrap();
        assert_eq!(nosep, separated);

        // 缺 offset 报错
        assert!(
            OffsetDateTime::parse_ext_with_offset("2024-01-05 21:45:06", WithOffsetParser::YmdHMS)
                .is_err()
        );
    }

    #[test]
    #[serial]
    fn test_parse_ext_local_unique() {
        let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
        let OffsetResult::Some(dt) =
            OffsetDateTime::parse_ext_local("2024-01-05 13:45:06", WithoutOffsetParser::YmdHMS)
                .unwrap()
        else {
            panic!("expected unique local datetime");
        };
        assert_eq!(dt, datetime!(2024-01-05 5:45:06 UTC));
        assert_eq!(dt.offset(), offset!(+8));

        let OffsetResult::Some(dt) = OffsetDateTime::parse_ext_local(
            "2024-01-05 13:45:06.789",
            WithoutOffsetParser::YmdHMS3F,
        )
        .unwrap() else {
            panic!("expected unique local datetime");
        };
        assert_eq!(dt, datetime!(2024-01-05 5:45:06.789_000_000 UTC));

        let OffsetResult::Some(dt) =
            OffsetDateTime::parse_ext_local("20240105134506", WithoutOffsetParser::YmdHMSnosep)
                .unwrap()
        else {
            panic!("expected unique local datetime");
        };
        assert_eq!(dt, datetime!(2024-01-05 5:45:06 UTC));

        // 格式非法报错
        assert!(
            OffsetDateTime::parse_ext_local("2024/01/05 13:45:06", WithoutOffsetParser::YmdHMS)
                .is_err()
        );
    }

    #[test]
    #[serial]
    fn test_parse_ext_local_dst_gap_and_overlap() {
        let _tz_guard = TzGuard::new(tz("America/New_York"));
        // 2024-03-10 02:00 EST 春季拨快到 03:00 EDT，02:30 的钟面时间不存在
        assert!(matches!(
            OffsetDateTime::parse_ext_local("2024-03-10 02:30:45", WithoutOffsetParser::YmdHMS)
                .unwrap(),
            OffsetResult::None
        ));
        // 2024-11-03 02:00 EDT 秋季拨回 01:00 EST，01:30 的钟面时间对应两个时刻
        let OffsetResult::Ambiguous(first, second) =
            OffsetDateTime::parse_ext_local("2024-11-03 01:30:45", WithoutOffsetParser::YmdHMS)
                .unwrap()
        else {
            panic!("expected ambiguous local datetime");
        };
        let offsets = [first.offset(), second.offset()];
        assert!(offsets.contains(&offset!(-4)) && offsets.contains(&offset!(-5)));
        assert_eq!(
            (first.unix_timestamp() - second.unix_timestamp()).abs(),
            3600
        );
    }

    // ---- format / shift ---------------------------------------------------

    #[test]
    #[serial]
    fn test_format_ext_local() {
        let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
        let dt = datetime!(2024-01-05 5:45:06.789_123_456 UTC);
        assert_eq!(
            dt.format_ext_local(WithoutOffsetFormatter::YmdHMS.into())
                .unwrap(),
            "2024-01-05 13:45:06"
        );
        assert_eq!(
            dt.format_ext_local(WithoutOffsetFormatter::YmdHMS3F.into())
                .unwrap(),
            "2024-01-05 13:45:06.789"
        );
        assert_eq!(
            dt.format_ext_local(WithOffsetFormatter::YmdTHMS3F.into())
                .unwrap(),
            "2024-01-05T13:45:06.789+08:00"
        );
    }

    /// TZ 优先；查不到就和没设一样，走系统设置
    #[test]
    fn test_detect_timezone_prefers_tz_env() {
        assert_eq!(
            detect_timezone(Some("Asia/Shanghai".into())),
            tz("Asia/Shanghai")
        );
        assert_eq!(
            detect_timezone(Some("America/New_York".into())),
            tz("America/New_York")
        );
        // glibc 的冒号前缀
        assert_eq!(
            detect_timezone(Some(":Asia/Shanghai".into())),
            tz("Asia/Shanghai")
        );
        let system = detect_timezone(None);
        assert_eq!(detect_timezone(Some(":/etc/localtime".into())), system);
        assert_eq!(detect_timezone(Some("Not/AZone".into())), system);
        assert_eq!(
            detect_timezone(Some("CST-6CDT,M3.2.0,M11.1.0".into())),
            system
        );
    }

    /// 贴近上下界、换到本地时区后越界：返回 DateError，不 panic
    #[test]
    #[serial]
    fn test_local_out_of_range_returns_err() {
        let max = OffsetDateTime::new_utc(Date::MAX, Time::MAX);
        let min = OffsetDateTime::new_utc(Date::MIN, Time::MIDNIGHT);
        let formatter = DateTimeFormatter::from_str("WithOffset.YmdHMS3F").unwrap();

        let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
        assert!(
            max.format_ext_local(formatter)
                .unwrap_err()
                .is(BaseErr::DateError)
        );
        assert!(max.start_of_day_local().unwrap_err().is(BaseErr::DateError));
        assert!(max.end_of_year_local().unwrap_err().is(BaseErr::DateError));
        let near_max = max - Duration::hours(1);
        assert!(
            near_max
                .shift_local(Duration::minutes(30))
                .unwrap_err()
                .is(BaseErr::DateError)
        );
        drop(_tz_guard);

        let _tz_guard = TzGuard::new(tz("America/New_York"));
        assert!(
            min.format_ext_local(formatter)
                .unwrap_err()
                .is(BaseErr::DateError)
        );
        assert!(
            min.start_of_month_local()
                .unwrap_err()
                .is(BaseErr::DateError)
        );
    }

    #[test]
    #[serial]
    fn test_shift_local() {
        let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
        let dt = datetime!(2024-01-05 5:45:06 UTC);
        let shifted = dt.shift_local(Duration::hours(8)).unwrap();
        // 结果转换到系统时区，与 UTC 表示同一时刻
        assert_eq!(shifted.offset(), offset!(+8));
        assert_eq!(shifted, datetime!(2024-01-05 13:45:06 UTC));
        // 负向平移跨过本地零点
        let shifted = dt.shift_local(-Duration::hours(1)).unwrap();
        assert_eq!(shifted, datetime!(2024-01-05 4:45:06 UTC));
    }

    // ---- 本地日历边界 ------------------------------------------------------

    #[test]
    #[serial]
    fn test_boundaries_local_shanghai() {
        let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
        // 2024-01-03 是周三
        let dt = datetime!(2024-01-03 15:30:45 +8);

        assert_local_eq(
            unique(dt.start_of_day_local()),
            datetime!(2024-01-03 0:00:00 +8),
        );
        assert_local_eq(
            unique(dt.end_of_day_local()),
            datetime!(2024-01-03 23:59:59.999_999_999 +8),
        );
        // 2024-01-01 是周一
        assert_local_eq(
            unique(dt.start_of_week_local()),
            datetime!(2024-01-01 0:00:00 +8),
        );
        assert_local_eq(
            unique(dt.end_of_week_local()),
            datetime!(2024-01-07 23:59:59.999_999_999 +8),
        );
        assert_local_eq(
            unique(dt.start_of_month_local()),
            datetime!(2024-01-01 0:00:00 +8),
        );
        assert_local_eq(
            unique(dt.end_of_month_local()),
            datetime!(2024-01-31 23:59:59.999_999_999 +8),
        );
        assert_local_eq(
            unique(dt.start_of_year_local()),
            datetime!(2024-01-01 0:00:00 +8),
        );
        assert_local_eq(
            unique(dt.end_of_year_local()),
            datetime!(2024-12-31 23:59:59.999_999_999 +8),
        );

        // 2 月：闰年 29 天 / 平年 28 天
        let leap = datetime!(2024-02-15 12:00:00 +8);
        assert_local_eq(
            unique(leap.end_of_month_local()),
            datetime!(2024-02-29 23:59:59.999_999_999 +8),
        );
        let common = datetime!(2023-02-15 12:00:00 +8);
        assert_local_eq(
            unique(common.end_of_month_local()),
            datetime!(2023-02-28 23:59:59.999_999_999 +8),
        );
        // 30 天的月
        let april = datetime!(2024-04-15 12:00:00 +8);
        assert_local_eq(
            unique(april.end_of_month_local()),
            datetime!(2024-04-30 23:59:59.999_999_999 +8),
        );
    }

    #[test]
    #[serial]
    fn test_start_of_next_local_shanghai() {
        let _tz_guard = TzGuard::new(tz("Asia/Shanghai"));
        // 2024-12-31 是周二
        let dt = datetime!(2024-12-31 15:30:45 +8);
        assert_local_eq(
            unique(dt.start_of_next_day_local()),
            datetime!(2025-01-01 0:00:00 +8),
        );
        assert_local_eq(
            unique(dt.start_of_next_week_local()),
            datetime!(2025-01-06 0:00:00 +8),
        );
        assert_local_eq(
            unique(dt.start_of_next_month_local()),
            datetime!(2025-01-01 0:00:00 +8),
        );
        assert_local_eq(
            unique(dt.start_of_next_year_local()),
            datetime!(2025-01-01 0:00:00 +8),
        );
        // UTC 还是 12-31，按本地时区已经是 2025-01-01：以本地日期为准
        let utc_evening = datetime!(2024-12-31 17:00:00 UTC);
        assert_local_eq(
            unique(utc_evening.start_of_next_day_local()),
            datetime!(2025-01-02 0:00:00 +8),
        );
        let max = OffsetDateTime::new_utc(Date::MAX, Time::MIDNIGHT);
        assert!(
            max.start_of_next_day_local()
                .unwrap_err()
                .is(BaseErr::DateError)
        );
    }

    /// 下一天的零点被夏令时跳过时，原样返回 `OffsetResult::None`，由调用方处理
    #[test]
    #[serial]
    fn test_start_of_next_day_local_dst_gap() {
        // America/Santiago 2024-09-08 零点拨快到 01:00，这一天的零点不存在
        let _tz_guard = TzGuard::new(tz("America/Santiago"));
        let dt = datetime!(2024-09-07 12:00:00 -4);
        assert!(matches!(
            dt.start_of_next_day_local().unwrap(),
            OffsetResult::None
        ));
    }

    #[test]
    #[serial]
    fn test_boundaries_local_new_york() {
        let _tz_guard = TzGuard::new(tz("America/New_York"));
        // 7 月处于 EDT（-04:00），当地零点 = 04:00 UTC
        let july = datetime!(2024-07-10 8:00:00 -4);
        let start = unique(july.start_of_day_local());
        assert_eq!(start, datetime!(2024-07-10 0:00:00 -4));
        assert_eq!(start, datetime!(2024-07-10 4:00:00 UTC));
        assert_eq!(start.offset(), offset!(-4));
        // 1 月处于 EST（-05:00），当地零点 = 05:00 UTC
        let january = datetime!(2024-01-10 7:00:00 -5);
        let start = unique(january.start_of_day_local());
        assert_eq!(start, datetime!(2024-01-10 0:00:00 -5));
        assert_eq!(start, datetime!(2024-01-10 5:00:00 UTC));
        assert_eq!(start.offset(), offset!(-5));
    }
}
