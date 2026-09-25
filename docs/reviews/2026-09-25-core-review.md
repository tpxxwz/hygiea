# hygiea-core 审查记录（2026-09-25）

## 范围

| 模块 | 文件 |
|---|---|
| error | `hygiea-core/src/error.rs`、`hygiea-macros/src/error.rs` |
| env | `hygiea-core/src/env.rs` |
| sync | `hygiea-core/src/sync/lock.rs`、`sync/mod.rs` |
| string | `hygiea-core/src/string/`、`hygiea/src/string.rs` |
| datetime | `hygiea-core/src/datetime/mod.rs`、`datetime/clock.rs` |
| app / log | `hygiea-core/src/app.rs`、`hygiea-core/src/log.rs` |
| facade | `hygiea/src/lib.rs` |

不在范围内：`net/ws.rs`、`sync/rate_limit.rs`（另行处理）。

## 约定

- **状态**：「已复现」= 在临时 crate 里实际跑出来；「已确认」= 读代码或依据库的语义可以确定；「推测」= 可能发生，未复现。
- **类别**：真 bug / 设计 / 测试缺口 / 风格。
- 每一项有编号，处理完把 `[ ]` 改成 `[x]`，并在后面注明 commit 或说明。
- 行号是审查当天的，代码改动后可能偏移，以描述为准。

## 建议处理顺序

1. **log / app 的日志丢失和失败被吞**：L1–L4、A1–A5。直接影响线上排查问题，改动集中。
2. **error 的启动期问题和宏问题**：E1–E6。
3. **string 的 `tpl_pos` 重写**：S1。一个函数，改动小。
4. **datetime**：D1–D6。SimClock 要重构，改动最大，建议单独做。
5. 其余设计问题和测试缺口，按需穿插处理。

---

## log（`hygiea-core/src/log.rs`）

### 真 bug

- [ ] **L1 默认配置每次重启都会删掉上次的日志**（:51，已复现）
  `max_log_files` 默认是 1，tracing-appender 构建时就会清理旧文件，只保留 n-1 个。服务崩溃后重启，崩溃前的 `app.log` 就没了。
  建议：默认改成不限制（`None`）或一个合理的值，并在文档里说明清理规则。
- [ ] **L2 同一目录下多个文件 layer 会互删日志**（:50/:171，已复现）
  清理旧文件时按「前缀 + 后缀」匹配，前缀为 `app` 的 layer 会删掉同目录下的 `app-error.log`。
  建议：文档注明前缀不能互相包含，或者在初始化时检测并报错。
- [ ] **L3 `root_dir` 配置完全不生效**（:62/:68，已确认）
  `init_tracing` 里没有用到 `root_dir` 和 `DEFAULT_ROOT_DIR`，日志只按各 layer 的 `dir`（默认 `./logs`）写。
  建议：实现它（`dir` 为相对路径时拼在 `root_dir` 下），或者删掉这个字段。
- [ ] **L4 filter 拼错会把所有日志过滤掉，且不报错**（:144/:180/:211，已复现）
  用的是 `EnvFilter::new`，`"infoo"` 会被当成 target 名处理，结果连 ERROR 都不输出。
  建议：改用 `EnvFilter::try_new` 或 `Builder::parse`，把错误返回给调用方。

### 设计

- [ ] **L5** 控制台 layer 不判断 stdout 是不是终端，始终输出 ANSI 颜色码（:141，已复现）。docker、journald 收集到的日志里会有转义字符。
- [ ] **L6** `console.disable = true` 且没有启用任何文件 layer 时，会悄悄回退成控制台输出，覆盖了用户的显式配置（:207-219）。
- [ ] **L7** `rolling` 用字符串表示，大小写敏感，写成 `"daily"` 就失败；配置结构也没有 `deny_unknown_fields`，拼错的键会被静默忽略（:153、:21-39）。建议改成 serde enum。
- [ ] **L8** `LogTracer::init` 在 `set_global_default` 之前执行，进程里如果已经装了 `log` logger（比如某个依赖装了 env_logger），整个初始化会失败（:221）。另外 `env_filter` 这个名字看起来像是会读环境变量，实际不读 `RUST_LOG`。
- [ ] **L9** `non_blocking` 默认是 lossy 模式，高负载时会静默丢日志；设为 `false` 时又会在 tokio worker 线程里同步写文件。两种取舍文档里都没写（:182）。

### 风格

- [ ] **L10** 两个分支除了 writer 以外完全重复，可以用 `BoxMakeWriter` 合并；`Option<Box<dyn Layer>>` 反复 `and_then` 拼接，改成收集到 `Vec<Box<dyn Layer>>` 更清楚；`time_format: Option` 的 `None` 分支和默认值行为一样，这个 Option 可以去掉；几个配置类型都没有 `Debug`（:182-204）。

---

## app（`hygiea-core/src/app.rs`）

### 真 bug

- [ ] **A1 `run()` 启动失败时退出码仍是 0**（:329-330，已复现）
  `run` 返回 `()`，启动失败只打一条日志，systemd、k8s 会当成正常退出。
  建议：返回 `Result<(), LaunchError>`。
- [ ] **A2 日志组件自己启动失败时，错误打不出来**（:239 + :330，已复现）
  比如 `rolling = "daily"` 导致 TracingComponent 启动失败，这时 tracing 还没装好，错误日志直接丢失：进程没有任何输出，退出码是 0。
  建议：tracing 初始化失败时退回 `eprintln!`。
- [ ] **A3 同一个启动错误被记录两次**（:367 + :330，已复现）
- [ ] **A4 错误只显示最外层 context**（:83，已确认）
  用 `{}` 格式化 anyhow 错误，组件里写的 `.context("connect db")` 只显示 "connect db"，看不到根因 "connection refused"。
  建议：改成 `{:#}`。
- [ ] **A5 日志 guard 释放得太早**（:218-220、:325，已复现）
  LogGuard 存在 Registry 里，`run` 返回时就随 Registry 一起 drop 了，并不像注释说的那样活到进程结束。结果是 `run().await` 之后在 main 里打的日志不会写进文件。另外 components 按注册顺序 drop，TracingComponent 排在第 0 个、最先被 drop，其他组件在 `Drop` 里打的日志也进不了文件。
- [ ] **A6 `--version` 打印的是 hygiea-core 的版本**（:23，已确认）
  `#[command(author, version, about)]` 在 hygiea-core 里展开，读到的是 hygiea-core 自己的 Cargo 元数据。
- [ ] **A7 配置错误的报错指向错误的文件，而且直接 panic**（:297/:299，已确认）
  错误信息里写死的是主配置文件名：`-f extra` 指定的文件出错时，panic 报的却是 `config/dev.toml`。`load_config` 遇到配置错误直接 panic，签名和文档都没有说明，应该返回 `Result`。

### 设计

- [ ] **A8** `AppCli::parse()` 独占了整个进程的命令行参数，应用自己定义的参数会被 clap 当成未知参数，直接 exit（:281）。
- [ ] **A9** 关闭流程并不是注释写的「逆序关闭」：关闭信号一次性广播给所有组件，逆序的只是 await 的顺序，HTTP 和它依赖的 DB 实际上是同时停止的（:381-395）。await 也没有超时；装上 `ctrl_c` 之后，有一个任务不响应 shutdown 时，第二次 Ctrl+C 也杀不掉进程。
- [ ] **A10** Component 没有 stop/shutdown 钩子，不起后台任务的组件（比如连接池）没有机会优雅关闭；后台任务提前退出或 panic 时没有监控，应用会一直停在 ready 状态；信号 handler 在所有组件启动完之后才安装，启动期间收到 SIGTERM 会直接杀掉进程（:193-214）。
- [ ] **A11** API 细节（:34/:139/:247）：
  - 名字限定为 `&'static str`，从配置里读到的名字得 leak 才能用；
  - Registry 里存了 `TypeId` 却从来没用上，重复 add 同类型同名的组件不会被发现；
  - `insert` 会静默覆盖同类型的资源；
  - `get` 要求 `T: Clone` 并返回克隆，`remove` 却返回 `Arc<T>`，两者不对称；
  - `run` 的回调是同步的 `FnOnce`，里面做不了 async 初始化。
- [ ] **A12** 文档问题（:41-42、:65、:269/:271）：
  - `RegistryConfig` 的文档说 tracing 字段是 Option，实际不是；
  - `Registry::from_file` 是个断链，实际的方法叫 `load_config`；
  - `load_config` 的文档第一行重复了两遍。
- [ ] **A13** 总是强制注册 TracingComponent，不能关闭，也不能换成用户自己的 subscriber；同一进程里的第二个 Registry 启动必然失败，对测试很不友好（:259-266）。

### 风格

- [ ] **A14** 其他风格问题：
  - components 用 4 元组存储，访问时写 `.1/.2/.3`，建议换成具名 struct；
  - `Resources` 手写的 `Clone` 可以直接 derive；
  - :174 的 `Arc::clone(&arc)` 多余；
  - 中英文注释混用；
  - clippy 的三条警告（:152 map_clone、:254 new_without_default、:306 should_implement_trait）都不影响正确性，其中 :306 是误报。

---

## error（`hygiea-core/src/error.rs`、`hygiea-macros/src/error.rs`）

### 真 bug

- [ ] **E1 错误码冲突时进程直接 abort**（error.rs:27-35、lib.rs:19，已复现）
  保留码和重复码的检查都放在 ctor 里 panic。ctor 里的 panic 不能 unwind，进程直接 SIGABRT（exit 134），附带一大段 "panic in a function that cannot unwind" 的栈。写 `err_code_prefix = "000"` 加 `err_code = "00000"` 能通过编译，但所有依赖它的测试二进制都会整体崩掉。
  建议：保留码检查挪到宏的 `validate` 阶段，在编译期报错；运行时的重复码检查改成清楚的报错，不要 abort。
- [ ] **E2 下游在自己的 `#[ctor]` 里格式化 HyErr 会 panic**（error.rs:92-94，已复现）
  错误模板只在 core 的 ctor 里初始化，`Display` 里直接 `expect`。ctor 之间的执行顺序没有保证。
  建议：改成 `OnceLock::get_or_init` 懒初始化。
- [ ] **E3 跨 crate 的错误码重复，在 macOS 上链接阶段不报错**（macros error.rs:365-372，已复现）
  ld64 只给出 `warning: duplicate symbol`，最后靠运行时 ctor abort 才拦住。`hygiea/tests/hy_err_ui.rs` 注释里写的「链接阶段才报」在 macOS 上不成立，要一起更正。
- [ ] **E4 中文命名的 enum 或变体编译失败**（macros error.rs:410-425，已复现）
  `sanitize_ident_part` 把非 ASCII 字符都换成 `_`，`用户错误::{未找到, 无权限}` 生成出来的标识符撞名，报 E0428，而且报错位置指向 derive，看不出原因。
  建议：生成的标识符改用错误码或 hash。
- [ ] **E5 MSRV 声明与实际不符**（macros error.rs:503-504，已确认）
  代码用了 let chains（1.88 才稳定），workspace 声明的是 `rust-version = "1.85"`，用 1.85–1.87 编译会失败。二选一：改掉 let chains，或者把声明调到 1.88。
- [ ] **E6 带字段的变体报错看不出原因**（macros error.rs:261，已复现）
  `A(u8)` 这种变体报 E0532，位置指向 derive。应该在宏里明确报「只支持 unit variant」。enum 的泛型参数也被忽略了。

### 设计

- [ ] **E7** 模板渲染失败时，兜底的 `Display` 文案会把 `err_tpl` 和完整的 `err_args` 带出来，和「内部细节不随 Display 外泄」的设计相矛盾（:97-101）。比如 key 拼错、缺字段时，`args` 里的 body 等内容会直接出现在返回给客户端的消息里。建议 Display 只输出固定文案加错误码，细节放到 `{:#}` 或 Debug 里。
- [ ] **E8** `err!` 的 const 断言要求 `$kind` 是常量表达式，`let k = ..; err!(k)` 会报 E0435，文档里没说（:167-200）。`{..}` 写法里的 key 只在渲染时才检查，而宏在编译期已经知道模板需要哪些变量，完全可以在编译期校验字面量 key。
- [ ] **E9** `undeclared_variables` 会把 minijinja 的全局函数也算成变量：模板 `range(n)` 得到的 vars 是 `["n","range"]`，结果 `err!(X, 3)` 被拒（macros :223）。
- [ ] **E10** `err_code`、`err_tpl`、`err_args` 都是 pub 可写字段，外部改了会破坏 `is()` 和 Display，建议改成只读访问器（:54-57）。`ERR_REGISTRATIONS` 和 `ErrRegistration` 当成普通公开 API 导出了，没加 `doc(hidden)`（lib.rs:16）。
- [x] **E11** 生成的代码写死了 `::hygiea::` 路径，下游重命名依赖，或者只依赖 `hygiea-core` 时会编译失败。可以考虑提供 `#[hy_err(crate = ..)]`。
  已处理：宏用 `proc-macro-crate` 按调用方的依赖名生成路径（`hygiea`、改名、只依赖 `hygiea-core` 都可以），并支持 `#[hy_err(crate = "..")]` 覆盖。

### 风格

- [ ] **E12** 生成的标识符还在用旧前缀 `WJJ_STD_`；同一个 `#[error]` 里重复写同一个 key 不报错，静默以最后一个为准。

---

## string（`hygiea-core/src/string/`、`hygiea/src/string.rs`）

### 真 bug

- [ ] **S1 `tpl_pos` 遇到正常的字面量也会出错**（template.rs:91-136，已复现）
  位置模板转成 minijinja 模板时，字面文本没有转义：
  - `"{{{}}}"`（按 Rust 语义是 `{值}`）、`"{%"`、`"a {# b"`、`"{{{{"` 都报语法错误；
  - `"{{{{ x }}}}"` 本意是输出字面量 `{{ x }}`，却被当作变量 `x` 求值；
  - null 参数渲染成 `"None"`。

  建议：直接做字符串替换，不再经过 minijinja。
- [ ] **S2 刚注册的模板可能被立刻淘汰**（template.rs:57-58，推测）
  `ensure_template_registered` 释放写锁之后才加读锁去取模板，中间如果其他线程插入了 1024 个新模板，刚注册的就会被淘汰，返回 `TemplateNotFound`。

### 设计

- [ ] **S3** 模板缓存和正则缓存命中时都用 `peek`，不更新顺序，实际是 FIFO 而不是注释写的 LRU（template.rs:78、pattern.rs:10/88）。
- [ ] **S4** 正则缓存未命中时，在持有全局写锁的情况下编译正则，遇到大正则会阻塞所有正则调用；非法正则不缓存，每次都会重新拿写锁（pattern.rs:91-95）。
- [ ] **S5** 替换串会展开 `$N`：`replace(r"\d", "a1", "$5 off")` 得到 `"a off"`。传入用户的原始文本时容易踩坑，也没有提供按字面量替换（`NoExpand`）的版本（pattern.rs:25-35，已复现）。
- [ ] **S6** `regex_find` 返回的是第 1 个捕获组，`find_all` 返回的是整体匹配，同样叫 find，语义却不一致；`regex_find_double` 这个名字也看不出含义（pattern.rs:70/77）。
- [ ] **S7** `fmt_tpl!` 和 `fmt_tpl_once!` 的参数是 `$args:tt`，传 `ctx.args` 这类表达式会报 "no rules expected `.`"（facade string.rs:6/14，已复现）。建议补一个 `$args:expr` 的分支。

### 风格

- [ ] **S8** 模板缓存每次命中都 `args.clone()` 做一次深拷贝，其实 `render(&args)` 就够了（template.rs:83）。

---

## env（`hygiea-core/src/env.rs`）

- [ ] **V1 设计** 只有 `env_get` 会回落到 `default_value`，`_opt`、`_or`、`_or_else` 都不看它，同一个 key 会得到三种结果（:152-165）。
- [ ] **V2 设计** `DefaultHome` 按 `USER` 拼路径，没有读 `HOME`（:105-121）。容器或 cron 里 `USER` 往往没设置，这时会 panic；家目录不在默认位置时，拿到的路径是错的。
- [ ] **V3 设计** 值不是 UTF-8 时被当成「未设置」处理，报错信息「not set」是错的（:146）。
- [ ] **V4 设计** `BuiltinKey` 里写死了业务相关的 key（AliCloud、AWS、Pg……），而且没有 `#[non_exhaustive]`，以后每加一个变体都是破坏性变更（:11）。

---

## sync/lock（`hygiea-core/src/sync/lock.rs`）

- [ ] **K1 设计** `DistributedKey` 没有 `Debug`、`Clone`、`PartialEq`、`Eq`、`Hash`，没法打日志，也不能当 map 的 key（:7）。
- [ ] **K2 设计** `debug_assert` 按字节数和 255 比较，但 PG 的 `VARCHAR(255)` 按字符计数，255 个字符以内的中文 key 在 debug 下也会误报；而且只在 debug 下检查，release 不检查（:44/:51）。
- [ ] **K3 设计** trait 方法没有文档，这些语义都没写：`try_lock` 返回 `Ok(None)` 表示什么、是否可重入、`f` panic 或 future 被取消时锁怎么释放、锁有没有超时；`T: 'static` 这个约束看起来多余（:59-74）。

---

## datetime（`hygiea-core/src/datetime/`）

### 真 bug

- [ ] **D1 SimClock 是进程级的全局单例，和推荐的并行方案冲突**（clock.rs:9-10/29-34，已复现）
  `SIM_TIME`、`SIM_QUEUE`、`NOW_FN` 都是进程级全局变量，而文档推荐的并行回测方案 A 是「每个线程一个独立的 paused runtime」。两个线程各自推进 2000 步时，`now_utc()` 分别有 2000 次和 1977 次和目标时间不一致。
- [ ] **D2 多个 SimClock 同时存在时，状态互相破坏**（clock.rs:29-34/69-75，已复现）
  新建第二个 SimClock 会清空第一个的队列；第二个被 drop 后，时钟退回真实时间，第一个随后的 `advance_to` 和 `advance_next` 都静默失效。
- [ ] **D3 SimClock 跟不上 tokio 自动推进的虚拟时间**（clock.rs:52-66，已复现）
  SimClock 的时间和 tokio 的虚拟时间是两套独立的状态，只在调用 `advance_to` 时同步。paused runtime 空闲时会自动推进：`sleep(1h).await` 之后 tokio 走了 3600 秒，`now_utc()` 还停在起点。依赖 `now_utc()` 的代码（比如 TokenBucket）可能因此卡住。
- [ ] **D4 runtime 没暂停时 `tokio::time::advance` 会 panic，而且会留下脏状态**（clock.rs:52-65，已确认）
  panic 之前 `SIM_TIME` 已经写成了新值。SimClock 没有任何前置检查，也不返回 `Result`。
- [ ] **D5 `*_local` 系列方法返回 `Result`，却会 panic**（mod.rs:925、:1012/:1017/:1029 等，已复现）
  `to_timezone` 内部调用 `to_offset`，结果越界时 panic。比如 `UtcDateTime::MAX` 调用 `format_ext_local`；`shift_local` 在接近 MAX 时也会 panic。
- [ ] **D6 读取系统时区时忽略 `TZ` 环境变量**（mod.rs:908-915，已确认）
  unix 上只读 `/etc/localtime` 这个符号链接。在 Docker 里设置 `TZ=Asia/Shanghai` 也不生效；`/etc/localtime` 是复制过来的普通文件，或者根本不存在时，会静默退回 UTC。
- [ ] **D7 带秒的时区偏移被截断（边缘情况）**（mod.rs:159/162/172，已确认）
  格式里只有 `[offset_hour]:[offset_minute]`：1880 年东京的 LMT 是 +09:18:59，格式化后再解析回来差 59 秒；而 RFC3339 格式化会直接返回 Err，两条路径表现不一致。
- [ ] **D8 chrono 闰秒往返不是恒等（极边缘）**（mod.rs:1476/1481，已确认）
  23:59:60.5 会被映射到下一天的 00:00:00.5。`from_utc_datetime` 不可能失败，返回 `Result` 没有必要。

### 设计

- [ ] **D9** SimClock 的文档写着「创建后 tokio 时间暂停」，但 `new` 并不会暂停 tokio，只是要求调用方事先暂停，文档前后矛盾（clock.rs:17-19）。
- [ ] **D10** SimClock 是 RAII guard，却没有 `#[must_use]`：`let _ = SimClock::new(t);` 会立刻 drop，恢复真实时钟，编译器不给任何警告（clock.rs:23）。
- [ ] **D11** `push` 一个早于当前时间的时间点时，`advance_next` 会静默丢弃它；`advance_to` 只额外 yield 一次，连锁唤醒的任务可能还没跑完函数就返回了（clock.rs:37-47，推测）。
- [ ] **D12** 在零点切换夏令时的时区，`start_of_day_local` 返回 `OffsetResult::None`（比如 America/Santiago 2024-09-08，那天实际从 01:00 开始），`end_of_day_local` 也可能返回 Ambiguous（mod.rs:1028-1042）。建议：遇到跳过的时段取切换后的第一个时刻，遇到重叠取靠后的那个。
- [ ] **D13** `DateTimeFormattable` 和 `HygieaDateTimeExt` 两个 trait 里都有 `format_ext`，`use hygiea::datetime::*` 之后调用 `dt.format_ext(..)` 会报 E0034 歧义（mod.rs:200-213 与 252-258）。
- [ ] **D14** 公开 API 返回了 `time_tz::OffsetResult`，但没有再导出，用户要 match 它就得自己加一个版本对得上的 time-tz 依赖（mod.rs:932、:955-993）。
- [ ] **D15** 所有 `*_local` 都只能用进程级的系统时区，没有接受显式时区的 `_in(tz)` 变体，满足不了交易系统按交易所时区（比如 America/New_York）切日的需求（mod.rs:903、:944-993）。
- [ ] **D16** `format_ext_local` 接受 `DateTimeFormatter`，而 `format_ext` 接受 `impl Into<DateTimeFormatter>`，参数类型不一致（mod.rs:960 与 256）。
- [ ] **D17** feature 耦合（mod.rs:927-941、:948）：
  - `WithoutOffsetParser` 总是被导出，但它的 `parse` 只在开启 datetime-iana 时才有；
  - `parse_ext_with_offset` 跟 IANA 无关，却放在了 iana trait 里。
- [ ] **D18** 格式化器和解析器不对称（mod.rs:64-76 与 47-51）：默认日志格式 `YmdTHMS3F` 没有对应的解析器；`WithOffsetParser::YmdHMSnosep` 没有对应的格式化器。
- [ ] **D19** `start_of_day` 这类方法不可能失败，却返回 `Result`；`end_of_*` 返回 23:59:59.999999999 这样的闭区间终点，容易诱导调用方写 `<=`，半开区间 `[start, next_start)` 更稳（mod.rs:268-292）。
- [ ] **D20** `shift_local` 只是把绝对时间平移后再换时区，名字却像是按本地时间计算，跨夏令时时「+1 天」实际是 23 或 25 小时（mod.rs:1015）。
- [ ] **D21** `FromStr` 的 `Err` 是 `String`，和库里统一用的 HyErr 不一致；也没有实现 Display 或 Serialize，配置没法回写（mod.rs:90-109）。

### 风格 / 其他

- [ ] **D22** 开启 sim-clock feature 后，每次 `now_utc()` 都要拿一次 RwLock 读锁；这个全局钩子在 `cfg(test)` 下也会编译进来，没标 `#[serial]` 却读时钟的单测，可能读到仿真时间（mod.rs:17-29）。
- [ ] **D23** clippy 报的 `too_many_arguments`（mod.rs:1163）只出现在测试 fixture 里。建议改用 `time::macros::datetime!`，顺便去掉 `local`、`utc_nano` 这些辅助函数。
- [ ] **D24** clock.rs:207-510 有大约 300 行纯 tokio API 的演示测试，不涉及 SimClock，更适合挪到 examples 或文档里。

---

## facade（`hygiea/src/lib.rs`、`hygiea/Cargo.toml`）

- [ ] **F1 设计** 文档里的 doctest 包在 `#[cfg(feature = "error")]` 下，而这个 feature 并不存在，所以示例从来没被编译过（lib.rs:19/66/71）。文档还列出了不存在的 `error`、`time` feature；标题仍是「WJJ Standard Library」，`html_root_url` 还是 0.0.1。
- [ ] **F2 风格** facade 直接依赖了 `serde_json`，但源码里没有直接用到，都是经过 `__private` 转接的。
- [ ] **F3** `hygiea-examples` 为了绕过 `hy_err` 的路径问题，额外加了 `linkme` 和 `serde_json` 两个依赖。这个问题已经在同期修掉了，这两个依赖可以删。

---

## 测试缺口汇总

- [ ] **T1** `app.rs`、`log.rs`、`env.rs`、`sync/lock.rs`、`error.rs` 本身都没有单元测试。app 和 log 装的是全局 subscriber，测试要放在独立的集成测试二进制里，或者用子进程跑。
- [ ] **T2** error：`{:#}` 的 source 链、渲染失败时的兜底输出、`wrap_err`、跨 enum 的 `is`、保留码和重复码检查、初始化之前调用 Display，都没有测试。
- [ ] **T3** app / log：组件启动失败后的回滚、关闭顺序、资源的覆盖和读取、`load_config` 的合并和报错、rolling 和 filter 取值的解析、文件 layer 实际写出的内容、旧日志清理，都没有测试。
- [ ] **T4** string：字面量里的 `{%`、`{#`、`{{{}}}`、缓存淘汰、并发、`$` 替换、非法正则返回的错误类型，都没有测试。
- [ ] **T5** datetime：
  - 跨夏令时的周界和月界、零点切换夏令时的时区、`shift_local` 跨夏令时；
  - 时间范围上下界附近的 `*_local`、带秒的时区偏移、9999 年的溢出路径；
  - 格式化后再解析的往返、glob import 的可用性；
  - SimClock 的多实例、多线程、tokio 自动推进、未暂停的 runtime、`let _ =` 立即 drop。
