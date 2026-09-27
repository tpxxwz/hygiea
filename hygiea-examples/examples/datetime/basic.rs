//! 日期时间：格式化 / 解析、日 / 周 / 月边界、系统本地时区。
//!
//! 这里用到的 `OffsetDateTime` / `UtcDateTime`都是 `time` crate 自己的类型，hygiea 只通过
//! `HygieaDateTimeExt` / `HygieaUtcDateTimeExt` / `HygieaOffsetDateTimeExt` 给它们扩展了方法。
//!
//! ```bash
//! cargo run -p hygiea-examples --example datetime_basic
//! ```

use hygiea::HyErr;
use hygiea::datetime::{
    HygieaDateTimeExt, HygieaOffsetDateTimeExt, HygieaUtcDateTimeExt, OffsetResult,
    WithOffsetFormatter, WithOffsetParser, WithoutOffsetFormatter, WithoutOffsetParser, now_local,
    now_utc,
};
use time::{OffsetDateTime, UtcDateTime};

/// `_local` 系列返回 `OffsetResult`：夏令时切换时钟面时间可能不存在或出现两次，库不替调用方选，
/// 原样返回。这里为了打印简单，歧义取较早的一个，实际业务按场景处理（见 `datetime` 模块文档）
fn fmt_local(result: Result<OffsetResult<OffsetDateTime>, HyErr>) -> Result<String, HyErr> {
    Ok(match result? {
        OffsetResult::Some(dt) => dt.format_ext_rfc3339()?,
        OffsetResult::Ambiguous(a, b) => format!(
            "{}（歧义，另一个时刻是 {}）",
            a.min(b).format_ext_rfc3339()?,
            a.max(b).format_ext_rfc3339()?
        ),
        OffsetResult::None => "（这个钟面时间因夏令时切换不存在）".to_string(),
    })
}

fn main() -> Result<(), HyErr> {
    // ===== 格式化 =====
    let utc = now_utc();
    println!("1. 格式化（当前 UTC 时刻）:");
    println!(
        "   带 offset:   {}",
        utc.format_ext(WithOffsetFormatter::YmdTHMS3F)?
    );
    println!(
        "   不带 offset: {}",
        utc.format_ext(WithoutOffsetFormatter::YmdHMS3F)?
    );
    println!("   RFC 3339:    {}\n", utc.format_ext_rfc3339()?);

    // ===== 解析 =====
    println!("2. 解析 \"2024-01-05T21:45:06+08:00\":");
    // UtcDateTime 换算到 UTC，OffsetDateTime 保留输入自带的 +08:00
    let as_utc = UtcDateTime::parse_ext_rfc3339("2024-01-05T21:45:06+08:00")?;
    let as_offset = OffsetDateTime::parse_ext_rfc3339("2024-01-05T21:45:06+08:00")?;
    println!(
        "   UtcDateTime（换算到 UTC）:     {}",
        as_utc.format_ext_rfc3339()?
    );
    println!(
        "   OffsetDateTime（保留 offset）: {}\n",
        as_offset.format_ext_rfc3339()?
    );

    // 带 offset 的固定格式解析（没有分隔符 T、没有毫秒）
    let with_offset = WithOffsetParser::YmdHMS.parse("2024-01-05 21:45:06 +08:00")?;
    println!(
        "   WithOffsetParser::YmdHMS -> {}\n",
        with_offset.format_ext_rfc3339()?
    );

    // ===== UTC 日历边界 =====
    println!("3. UTC 日历边界（以当前 UTC 时刻为基准，半开区间 [start, next)）:");
    println!(
        "   今天: [{}, {})",
        utc.start_of_day().format_ext_rfc3339()?,
        utc.start_of_next_day()?.format_ext_rfc3339()?
    );
    println!(
        "   本周: [{}, {})",
        utc.start_of_week()?.format_ext_rfc3339()?,
        utc.start_of_next_week()?.format_ext_rfc3339()?
    );
    println!(
        "   本月: [{}, {})\n",
        utc.start_of_month()?.format_ext_rfc3339()?,
        utc.start_of_next_month()?.format_ext_rfc3339()?
    );

    // ===== 系统本地时区（feature datetime-iana）=====
    println!("4. 系统本地时区:");
    let local = now_local();
    println!(
        "   当前本地时间: {}",
        local.format_ext_local(WithOffsetFormatter::YmdTHMS3F.into())?
    );
    println!(
        "   本地今天: [{}, {})",
        fmt_local(local.start_of_day_local())?,
        fmt_local(local.start_of_next_day_local())?
    );
    println!(
        "   本地本周: [{}, {})",
        fmt_local(local.start_of_week_local())?,
        fmt_local(local.start_of_next_week_local())?
    );
    println!(
        "   本地本月: [{}, {})",
        fmt_local(local.start_of_month_local())?,
        fmt_local(local.start_of_next_month_local())?
    );

    // 不带 offset 的钟面时间按系统时区解析，同样可能唯一 / 歧义 / 不存在
    let parsed_local =
        OffsetDateTime::parse_ext_local("2024-06-01 08:00:00", WithoutOffsetParser::YmdHMS);
    println!(
        "   本地钟面时间 \"2024-06-01 08:00:00\" 解析: {}",
        fmt_local(parsed_local)?
    );

    Ok(())
}
