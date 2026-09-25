# date 模块设计审查

本文记录当前 `hygiea-core/src/date.rs` 的设计风险和后续建议。这里只记录问题，不代表这些项目都必须立即修改。

## 优先处理

### 1. 本地时区探测失败会回退到 UTC

`iana::local_timezone()` 探测系统时区失败时会使用 UTC 作为 fallback。此时 `*_local` 方法仍然可能成功返回结果，但这个结果不一定是系统真实的本地 IANA 时间。

需要在“保证本地语义准确”和“允许安全 fallback”之间做出明确选择：

- 让时区初始化失败向本地 API 传播；或
- 启动时直接 fail-fast；或
- 明确把 fallback UTC 作为公开文档中的降级语义。

### 2. 模拟时钟不保证 `now_utc()` 的 offset 为 UTC

`set_now_utc` 接受 `fn() -> OffsetDateTime`，调用方可以返回携带非零 offset 的值。这样 `now_utc()` 名义上是 UTC，实际却可能返回 `+09:00` 等 offset。

需要决定是否在 `now_utc()` 返回前强制：

```rust
datetime.to_offset(UtcOffset::UTC)
```

或者在 `set_now_utc` 时校验 callback 的返回值。

### 3. IANA feature 绑定了不依赖 IANA 的能力

`HygieaOffsetDateTimeExt::parse_ext_with_offset` 只使用 `WithOffsetParser`，输入本身携带 offset，并不需要 `time-tz`。但目前整个 trait 位于 `date-iana` 模块中，因此启用 IANA feature 才能使用这个方法。

可以考虑把它移到始终可用的 OffsetDateTime 扩展中，或者直接删除这个对 `WithOffsetParser::parse` 的重复转发方法。

## API 设计问题

### 4. `WithoutOffsetParser` 在关闭 feature 时仍然导出，但不能 parse

`WithoutOffsetParser` 类型本身始终存在，`parse` 实现却只在 `date-iana` 开启时存在。关闭 feature 后它仍然叫 Parser，但没有可用的解析方法，API 完整性较弱。

可选方案：

- 将 `WithoutOffsetParser` 整体归入 `date-iana`；或
- 明确把它定义为“无 offset 的格式描述”，另提供 IANA 解析扩展。

### 5. 系统本地时区不是业务时区

当前所有 `_local` 方法使用系统检测到的 IANA 时区。这适合服务器本地时间，但不适合用户时区、租户时区或报表时区。

后续可以增加：

```rust
start_of_day_in(&self, timezone: &'static Tz)
```

再让 `start_of_day_local()` 作为系统时区快捷方法。这样仍然直接使用 `time_tz::Tz`，不需要 Hygiea 自定义包装类型。

### 6. ctor 中提前初始化本地时区可能过早

`date-iana` 开启时，crate ctor 会在应用 `main` 之前初始化系统时区。如果应用在启动配置阶段才设置 `TZ` 或其他时区来源，缓存可能已经固定。

`OnceLock` 本身支持惰性初始化，可以考虑删除 ctor 中的预初始化，仅在第一次调用本地 API 时解析时区。

### 7. `end_of_*` 的最后一纳秒语义

`end_of_day`、`end_of_month` 等返回 `23:59:59.999999999` 或相应周期最后一纳秒。对于微秒、毫秒精度的数据库，这通常需要截断或转换。

数据库查询更稳妥的形式通常是半开区间：

```text
[start_of_period, start_of_next_period)
```

`end_of_*` 可以保留，但查询层不应依赖它作为唯一边界表达。

## 测试与并发风险

### 8. 日期测试目前全部被注释

`date.rs` 中原有测试代码已删除。重新补测试时，应至少覆盖：

- DST 前跳产生的 `OffsetResult::None`；
- DST 回拨产生的 `OffsetResult::Ambiguous`；
- 午夜发生时区切换的地区；
- 月末、年末和时间类型边界；
- RFC3339 offset 保留与 UTC 转换；
- 模拟时钟返回非 UTC offset 的行为。

### 9. 全局模拟时钟的所有权和并发

`NOW_FN` 是全局可写 callback。多个 `SimClock` 嵌套或并发创建时，后创建的时钟会覆盖前一个，析构时又会无条件恢复真实时钟。

当前适合串行测试，不适合并发使用。后续可以考虑 guard/token 所有权模型，或明确限制只能存在一个活动模拟时钟。

## 当前结构中没有问题的部分

- `UtcDateTime` 的普通日历边界与 `OffsetDateTime` 的 IANA `_local` 边界分开，语义是清楚的。
- `OffsetResult` 直接使用 `time-tz` 原始类型，没有重新定义 Hygiea 包装枚举。
- `date-iana` 只负责可选 IANA 能力，底层依赖仍由 Hygiea 自己声明为 optional。
- `now() -> UtcDateTime`、`now_utc() -> OffsetDateTime`、`now_local() -> OffsetDateTime` 的类型职责是可区分的。
- 所有扩展方法使用 `&self` 并返回新值，不会修改原始日期对象。
