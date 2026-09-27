# 测试补全计划

范围：hygiea-core、hygiea-macros、hygiea-test-support、facade 的 trybuild、四个组件 crate、hygiea-examples。
每项标注放在哪里：**单元**（源文件的 `#[cfg(test)] mod tests`）、**集成**（`<crate>/tests/`）、**trybuild**（`hygiea-macros/tests/`，fixture 经 facade 使用宏）。

## 一、通用约定（补测试时统一遵守）

- 按模块、按功能组织：一个测试只测一件事，名字说清楚行为，比如 `require_missing_returns_resource_missing`，不用 `test_xxx_1` 这类名字。
- 装全局状态的测试放进 `tests/`，每个文件是一个独立进程：
  - `log::init` / `Registry::with_config` 会装全局 subscriber，并占用 `INSTALLED` 标记。lib 的单元测试里**不要**建 `Registry`，否则 `log.rs` 里断言 `!INSTALLED` 的测试会随机失败。
  - 会改环境变量（`set_var`）的测试放进 `tests/`，用唯一的变量名或者串行执行。
- 临时文件和目录统一用 `tempfile`（`TempDir` / `NamedTempFile`），drop 时自动删除。不要"先断言、最后再删"，否则断言失败时会留下文件。
- 读系统本地时区的测试必须标 `#[serial]`，并且用 override 固定时区，不依赖机器的 TZ。
- 同一件事只在一层测：单元测试已经覆盖了的逻辑，集成测试只保留一条端到端的用例。
- 需要真实服务（PostgreSQL / Redis）或联网的测试标 `#[ignore = "..."]`，文件头写上手动运行的命令。

## 二、需要先决定的问题

| # | 问题 | 影响 |
|---|---|---|
| D1 | ~~`AxumHttpError` 的 HTTP 状态码固定是 200~~ **已定：有意的**，业务错误一律 200，靠 body 里的错误码表示 | 测试断言错误响应是 200，并检查 body 里的 code 和 msg |
| D2 | ~~`apply_layers` 是私有函数~~ **已定：暴露**。提供一个公开的 router 构造函数（比如 `pub fn build_router(config) -> Router`），集成测试用 `tower::ServiceExt::oneshot` 直接测 413 / 408，不用占端口 | B9 先加这个函数再写测试 |
| D3 | ~~reqwest 默认的重试行为~~ **已定：用测试固定下来**。服务端计数，确认 5xx、连接被重置时实际发了几次请求 | B6 多一组集成测试 |
| D4 | ~~是否引入 dev-dependency `tempfile`~~ **已定：引入**。workspace 依赖里已经加了 `tempfile = "3.27.0"`，各 crate 在补测试时按需加到 `[dev-dependencies]`（`tempfile = { workspace = true }`） | 临时文件和目录统一用 `tempfile::TempDir` / `NamedTempFile`，不再手动拼路径、手动删除 |
| D5 | ~~test-support 新增 `Reply::bytes`~~ **已定：新增**。`Reply::bytes(status, content_type, Vec<u8>)` 构造非 UTF-8 响应；`Captured::text()` 里的 unwrap 改成 lossy 转换 | B6 先改 test-support 再写 http_client 的非 UTF-8 端到端测试 |
| D6 | ~~`ws_client.rs` 的测试~~ **已定：这次不补**。等恢复这个模块时，再用本地 tokio-tungstenite echo server 写 | 本计划不含 ws |
| D7 | ~~`duplicate_err_code_same_module` 的快照依赖宏生成的常量名~~ **已定：保留**。每次 derive 各自展开，宏看不到别的 enum 用了哪些码，没法自己报错；同模块撞码靠 `HYGIEA_ERR_CODE_<码>` 常量重名由 rustc 报 E0428，这是有意的机制。fixture 里已写明原因 | 无 |

## 三、现有测试要调整的

| 位置 | 问题 | 处理 |
|---|---|---|
| `log.rs` 里"init 失败……所以串行"那段注释 | 放在不调用 init 的测试前面了 | 挪到 init 相关的测试前 |
| `log.rs` 的 `test_init_file_overlap` 和 `test_init_failed_removes_created_dirs` | 都是"重叠 → 清理 → 还回标记"，只有根目录原本存不存在这一点不同 | 合并，或者改名写清区别 |
| `log.rs` 的 `test_time_config` | 实际测的是反序列化 | 改名 |
| `tests/log_file_layer.rs` | 名字里没体现"第二次 init 返回 InstallFailed" | 拆成两个断言清楚的步骤，或者改名 |
| `log.rs` 和 `tests/log_*.rs` 里的临时目录 | 断言失败会留下文件；有一处没用 `temp_root` | 统一用 RAII |
| `error.rs` 的 `registered_templates_load` | 名字说的是"没有重复"，实际只取了两个模板 | 改成遍历所有注册项、断言 `build_templates` 返回 Ok，并改名 |
| `string/template.rs` 的 `test_tpl_cached_hits_cache`、`string/mod.rs` 的 `test_fmt_tpl_cached_reuse` | 名字说"命中缓存"，实际只断言了渲染结果；和另一个测试重复 | 改成断言缓存里有这一项、`len` 不变 |
| `string/template.rs` 的缓存上限测试 | 会把全局缓存冲满，影响其他测试 | 标 `#[serial]`，或者改成只测淘汰函数 |
| `datetime/local.rs` 的 `test_parse_ext_with_offset` | 和 `layout.rs` 里的一份几乎一模一样 | 删掉 local 那份 |
| `datetime/local.rs` 的 `LOCAL_TIMEZONE_OVERRIDE` | 全局的 RwLock，新测试漏标 `#[serial]` 就会随机失败 | 在测试辅助代码上加注释说明 |
| `hygiea-http-client/tests/reqwest_client`：`masking.rs` 的 `non_2xx_logs_raw` / `decode_failure_logs_raw`、`send.rs` 的 `send_decode_failure_is_json_error`、`logs.rs` 的两处 | 和 `logging.rs` 里的单元测试重复 | 集成层只保留一条端到端的用例 |
| `hygiea-http-client/tests/reqwest_client`：`client_config.rs` 的 `is_timeout` 和 `send.rs` 的 `reqwest_source` | 同一个辅助函数写了两份 | 挪到 `support.rs` |
| `hygiea-http-client/tests/reqwest_client/client_config.rs` 的超时测试 | 用 50ms 超时配 300ms 延迟，CI 很慢时可能不稳 | 延迟拉大到 2s |
| `hygiea-db` 的 `sqlx_sqlite.rs` 测试 | 设了可能多余的 `shared_cache: true`；用的是 `&pool.inner`，没走对外的 Deref | 去掉，或者单独写一个测试说明为什么需要；改成用 `&*pool` |
| `hygiea-db` 的 `seaorm_postgres.rs` 测试 | "没有参数时不带问号"和默认 url 的测试重复；Deref 测试只检查了能编译，没有断言 | 合并；删掉 Deref 测试，或者补上断言 |
| `hygiea-macros/tests/hy_err_ui.rs`、`redact_ui.rs` | fail 和 pass 放在一个测试函数里，失败时不好定位 | 拆成两个函数 |

## 四、按 crate、按模块补测试

### hygiea-core / error

- **单元**
  - `with_source` 调用两次，以后一次为准；
  - `wrap_err` 在 `Ok` 时不调用闭包（用计数器验证）；
  - 渲染失败且带 source 时 `{:#}` 的拼接顺序；
  - 3 层及以上 source 链的 Debug 编号；
  - `source()` 能 downcast 回原始错误；
  - `bail!` 会提前返回；
  - `err!` 的三种形式：无参、单个值、`{..}`。
- **集成**：`tests/error_duplicate_code.rs`。用一个 fixture 程序注册两个同码的错误，通过子进程运行它，断言退出码是 1，stderr 包含 `hygiea: duplicate err_code`。trybuild 测不到这种运行期检查。

### hygiea-core / string

- **单元**
  - `tpl_once` 不写缓存；
  - `fmt_pos!` 的参数是表达式时；
  - `fmt_tpl!` 带尾逗号；
  - `replace_literal` / `replace_all_literal` 里的 `$` 不被展开（成对测）；
  - 空串和 Unicode 输入；
  - pattern 的正则缓存如果有上限，补淘汰相关的测试。

### hygiea-core / env

- **单元**
  - 变量不存在时 `env_get_opt` / `env_get_or` / `env_get_or_else` 的行为；
  - `env_get_or_else` 在变量存在时不调用闭包；
  - `BuiltinKey` 各变体的 `key_name` 对照表。
- **集成**：`tests/env.rs`。用 `set_var` 验证"环境变量优先于 key 自带的默认值"，每个用例用唯一的变量名。

### hygiea-core / redact

- **单元**
  - `Vec<带打码字段的 struct>`、`HashMap<K, 带打码字段的 struct>`、`Option<嵌套 struct>`（打码字段在内层）；
  - `#[serde(tag)]`、`tag + content`、`untagged` 的枚举；
  - mask 字段本身是 `Vec` 或 struct 时，整体替换成占位符；
  - `skip` 用在 `Option` 字段上；
  - 普通的 `serde_json::to_string` 不打码。
- **集成**：`hygiea-macros/tests/redact_runtime.rs`，经 facade 的 `hygiea::` 路径派生，断言 `to_redacted_json` 的实际输出。trybuild 的 pass 用例只能保证编译通过，验证不了输出。

### hygiea-core / datetime

- **单元（`local.rs`）**
  - Santiago 秋季回拨那天的 `end_of_day_local` 应得到 `Ambiguous`；
  - 纽约 3-10、11-03 两天的 `start_of_day_local` 都是 unique，但 offset 不同；
  - `shift_local` 跨夏令时切换时按绝对时长平移，offset 跟着变；
  - 同一时刻用 +8 和 -5 两种 offset 输入，`_local` 系列的结果相同；
  - 纽约回拨（overlap）当天的 `format_ext_local`。
- **单元（`utc.rs`）**
  - 跨年周：2024-12-31 的 `start_of_week`；2020-12-31 的 `end_of_week`；
  - 2024-02 和 2100-02 的 `end_of_month`（后者不是闰年）；
  - `shift` 跨月、跨年；
  - 负时间戳，以及 1970 年之前带小数部分的 millis。
- **单元（`layout.rs`）**
  - 解析失败的情况：2023-02-29、24:00:00、末尾多出字符、空串、`3F` 格式只给 2 位小数、`nosep` 位数不够、offset 写成 `+8:00`；
  - 所有 Parser 做 format → parse 往返后相等；只能格式化的 formatter 没有对应的 parser；
  - `DateTimeFormatter` 反序列化非法字符串时报错。
- **集成（可选）**：`tests/datetime_tz_env.rs`，用子进程设置 `TZ=:Asia/Shanghai` 和 `TZ=Bad/Zone`，验证 `now_local` 确实读了环境变量（override 覆盖不到这条路径）。

### hygiea-core / log

- **单元**
  - 被 disable 的 layer 即使 filter 写错也不报错；
  - `root_dir` 为空时用默认目录；
  - `layer_name` 的格式。
- **集成**（一种配置一个文件）
  - `tests/log_max_files.rs`：`max_log_files = 2` 时旧文件被清理，前缀不重叠的其他文件不受影响；
  - `tests/log_layer_filter.rs`：两个文件 layer 各自的 filter；layer 不配 filter 时沿用 root；disable 的 layer 不产生文件；
  - `tests/log_ansi.rs`：文件输出里永远不出现 `\x1b[`；设了 `NO_COLOR` 且 `ansi` 未配置时，`effective_ansi()` 为 false；
  - `tests/log_init_default.rs`：`init_default` 能成功。

### hygiea-core / app

- **单元（`resources.rs`）**
  - 匿名资源和具名资源互相独立；不同类型同名也互不影响；
  - 重复 `insert` / `insert_named` 会 panic；
  - `require` 在资源缺失时返回 `ResourceMissing`，信息里带类型名和资源名；
  - `remove` 之后 `contains` 为 false；
  - `ResourceId::of::<T>()` 等于 `named::<T>("")`，Hash 和 Display 的格式。
- **单元（`config.rs`）**
  - `parse_override`：正常解析、key 去掉空格、value 里可以含 `=`、`=v` 和 `noeq` 报错；
  - `resolve_env` 里 `--env` 优先的分支；
  - `RegistryConfig` 的默认值；遇到未知字段报错。
- **单元（`registry.rs`）**：只测纯函数 `find_cycle`，包括自环、3 个组件成环、环外还有别的组件。其他测试都放进 `tests/`。
- **集成（`tests/app_registry.rs`）**：进程里第一个 Registry 真正装上日志，之后的都走 InstallFailed 分支，所以多个测试可以放在同一个文件。用测试组件往共享的列表里记录事件：
  1. 依赖排序，以及没有依赖关系时保持 add 的顺序；
  2. 缺依赖报 `ResourceMissing`、两个组件提供同一资源报 `DuplicateProvider`、依赖成环报 `DependencyCycle`（信息里带环的路径），这三种情况都不应调用任何 startup；
  3. 启动失败时，已经启动的组件按逆序 stop，返回 `ComponentStartFailed`；
  4. 声明了 provides 但没有插入资源：报错，并且这个组件自己也会被 stop；
  5. `run` 的回调返回 Err 时，所有组件逆序关闭，`run` 返回同一个错误；
  6. 后台任务提前退出或 panic 时，返回 `TaskExited`，并关闭所有组件；
  7. 某个组件 stop 超时：它被 abort、打一条 WARN，下一个组件照常关闭；
  8. 组件自己的 `shutdown_timeout` 优先于全局配置；
  9. stop 返回 Err 时只打 WARN，不影响后面的组件；
  10. `add_named` 同类型同名重复会 panic，不同名可以共存。
- **集成（`tests/app_registry_shutdown.rs`，只放一个测试）**：给当前进程发 SIGTERM 触发退出，用暂停的 tokio 时间验证 `shutdown_delay` 结束前组件没有被 stop。
- **集成（`tests/app_load_config.rs`，每个测试用不同的 `env_prefix`）**
  - 分层合并：环境文件 < `-f` 指定的文件 < 环境变量 < `--set`；
  - `<PREFIX>_ENV` 选择环境；`-d` 和 `default_config_dir` 实际读取的路径；
  - 环境变量里的数字和布尔值能正确解析；
  - 这些情况会 panic：文件缺失、类型不对、未知字段（`#[should_panic]`）。

### hygiea-core / http_client

- **单元**
  - `error.rs`：各构造函数的错误码和参数；source 的 `{:#}` 里不包含 query 原文；`NonSuccessStatus` 的 body 不出现在对外的 Display 里；
  - `client.rs`：proxy 配置非法时 build 返回 `ClientBuildFailed`（这个错误码目前没有测试）。
- **集成**
  - `client_config.rs`：read_timeout（服务端发完头再慢慢发 body）；用 5xx 或者重置连接的方式确认请求次数（D3）；connect_timeout 要用不可路由的地址，标 `#[ignore]`；
  - `send.rs` / `masking.rs`：GBK 编码的 `String` 响应端到端；响应含非法字节时日志用 lossy 转换；非 2xx 的 GBK body 进入错误信息；bearer / basic 凭据不出现在日志和错误里；重定向之后 URL 参数照样打码（D5）；
  - `logs.rs`：`enable_logging(false)` 时流式响应不打日志。

### hygiea-macros

- **单元**（hygiea-macros 源文件里的 `mod tests`）
  - `is_numeric_with_len`；
  - `krate::parse_crate_arg`：合法、非法、不是 crate 这个键；
  - redact 的 `take_redact_attr`、`serde_metas`、`lit_str`：输入 syn 片段，断言返回 Ok 或 Err；
  - 泛型 helper 的前缀生成（`name::<..>::`）。
- **trybuild**（`hygiea-macros/tests/{hy_err_ui,redact_ui}/fail/`），逐个补上还没覆盖的报错分支：
  - hy_err：值不是字面量（`err_code = FOO`）；`#[error(err_code)]` 解析失败；`#[hy_err(crate = 123)]`；
  - redact：`#[redact()]` 为空；serde 的 `with` / `serialize_with` 写成非字符串；`#[redact(crate = 1)]`。
- **集成**
  - `hygiea-macros/tests/hy_err_facade.rs`：经 `hygiea::` 路径派生，断言 `err_code`、Display 和 `{:#}`；
  - `hygiea-core/tests/hy_err_internal.rs`：不写 `crate = ...`，依赖自动解析路径的情况。

### hygiea-test-support

- **单元**
  - `http_server.rs`：`decode_chunked`（多块、带扩展字段、长度写错、数据截断），`read_request` 解析 query 和 chunked；
  - `headers.rs`：名字统一转小写；同名时后者覆盖前者；
  - `logs.rs`：`capture` 只收 INFO 及以上的日志。

### hygiea-db

- **单元（`sqlx_postgres.rs`，目前一个测试都没有）**
  - 默认 url；只有 search_path；只有 params；两者都有时用 `&` 连接；
  - 默认值（30 / 600，lazy = false）；
  - `connect_lazy = true` 时连一个不存在的主机也能成功（不需要真实服务）。
- **单元（`sqlx_sqlite.rs`）**：`params = "?a=b"` 时去掉开头的 `?`；文件路径转换成 url。
- **集成（`tests/sqlite_component.rs`，用内存库）**
  - `provides` 返回的 ResourceId 带组件名；
  - startup 之后能用 `get_named` 取到连接池；
  - stop 之后连接池已关闭；stop 调用两次不报错；
  - 通过 Registry 注册 primary 和 replica 两个实例。
- **集成（`tests/pg.rs`，标 ignore）**：sqlx 和 seaorm 两种连接池的 connect、search_path 生效、stop。

### hygiea-redis

- **单元（`fred_pool.rs`，不连 Redis）**
  - `build_fred_config` 的三种 mode；cluster / sentinel 模式下 nodes 为空时报错；mode 非法时报错；空的用户名和密码转成 None；cluster 模式不设 database；
  - perf 和 connection 配置里的 0 表示沿用 fred 的默认值；
  - 未知字段报错。
- **集成（`tests/redis.rs`，标 ignore）**：connect、set/get、组件的 startup → stop、选择 db。

### hygiea-http

- **单元（`axum_server.rs`）**
  - ~~失败响应的 JSON 结构、999 框架错误对外换成 SysErr、`From<anyhow::Error>` 的两条路径~~（已补）；
  - 成功响应的 JSON 结构；
  - `addr()`；配置的默认值和反序列化。
- **集成**
  - `tests/axum_layers.rs`：body 超限返回 413、handler 太慢返回 408、`arity0` / `arity1` 的成功和失败（失败时状态码是 200，body 里是错误码）、请求的 JSON 格式不对（用公开的 router 构造函数 + `oneshot`）；
  - `tests/axum_component.rs`：起在随机端口，用 reqwest 请求一次；取消之后 JoinHandle 能正常结束。

### hygiea-grpc

- **单元（`tonic_server.rs`）**：缺少 `serve_fn` 报 `ConfigMissing`；地址非法报 `InvalidConfig`；端口被占用报 `BindFailed`；`shutdown_timeout` 的换算。
- **集成（`tests/tonic_serve.rs`）**：自定义一个收到取消信号就退出的 `serve_fn`，验证取消后能正常 join；可选：注册 tonic-health 服务，用客户端调用一次。

## 五、examples

现有 8 个示例和 README 的示例表一致，都不连外部服务。建议新增（每个都要同时加 `[[example]]` 并同步 README 的示例表）：

| 示例 | 内容 |
|---|---|
| `log_basic` | 控制台输出和按时间滚动的文件输出、filter、UTC 时间 |
| `datetime_basic` | 格式化和解析、日 / 周 / 月边界、本地时区 |
| `env_basic` | `BuiltinKey`、自定义 `EnvKey`、四个取值函数 |
| `redact_basic` | `#[redact(mask/skip)]`、`to_redacted_json` |
| `http_client_basic` | 请求本进程里起的 axum 服务，演示打码后的日志 |
| `http_axum_response` | `AxumHttpResponse` / `AxumHttpError`、body 大小限制 |

可选：`string_template` 里补上正则缓存，并改名为 `string_basic`。grpc 和 redis 需要额外依赖或外部服务，不加示例。

## 六、执行批次

每一批改的文件互不重叠，可以分别交给不同的 agent 并行做。每批完成后只跑对应的测试命令。

| 批次 | 内容 | 验证命令 |
|---|---|---|
| B1 | 第三节里 core 的调整：log、error、string、datetime | `cargo test -p hygiea-core --all-features --lib` 加上改动的 `--test` |
| B2 | error、string、env、redact 的单元测试，以及 env、error_duplicate_code 的集成测试 | `cargo test -p hygiea-core --all-features --lib error:: string:: env:: redact::`，加上对应的 `--test` |
| B3 | datetime 的单元测试和可选的集成测试 | `cargo test -p hygiea-core --all-features --lib datetime::` |
| B4 | log 的单元测试，以及 4 个 log 集成测试文件 | `cargo test -p hygiea-core --features log --lib log::`，加上对应的 `--test` |
| B5 | app 的单元测试，以及 3 个 app 集成测试文件 | `cargo test -p hygiea-core --features app --lib app::`，加上对应的 `--test` |
| B6 | test-support（先加 `Reply::bytes`、`Captured::text()` 改 lossy），再做 http_client 的调整和补充 | `cargo test -p hygiea-http-client --features reqwest --test reqwest_client`、`cargo test -p hygiea-test-support` |
| B7 | macros 的单元测试、trybuild、两个 hy_err 集成测试和 redact_runtime | `TRYBUILD=overwrite cargo test -p hygiea-macros --test hy_err_ui --test redact_ui`，检查快照的 diff |
| B8 | hygiea-db、hygiea-redis | `cargo test -p hygiea-db --all-features`、`cargo test -p hygiea-redis --all-features` |
| B9 | hygiea-http（先加公开的 router 构造函数）、hygiea-grpc | `cargo test -p hygiea-http --all-features`、`cargo test -p hygiea-grpc --all-features` |
| B10 | examples 和 README 的示例表 | `cargo build -p hygiea-examples --examples`，并逐个运行新增的示例 |

B1 和 B2、B3、B4 都会改 core 里的同一批文件，需要按顺序做，或者先做 B1。

## 七、执行结果（2026-09-27）

B1–B10 全部完成。和计划不同、或者需要后续决定的地方：

| 项 | 情况 |
|---|---|
| 运行期错误码重复检查（`error_duplicate_code`） | 没做。重复检查在进程启动时的 ctor 里执行，发现重复就 `exit(1)`，要测只能另编一个注册了重复码的程序再用子进程跑，代价较大。`check_codes` / `build_templates` 的重复拒绝逻辑已有单元测试 |
| `effective_ansi()` 在 `NO_COLOR` 下为 false | 没做，`effective_ansi` 是私有方法，集成测试访问不到；只测了"文件输出不含 ANSI 转义" |
| proxy 非法时 build 返回 `ClientBuildFailed` | 当前 reqwest 版本下非法 proxy 在构造 `Proxy` 时就报错，走不到 build。改用非法 user_agent 触发同一错误码 |
| `WithOffsetParser` 没有对应 formatter 的断言 | 两个枚举之间没有转换 API，只能在测试注释里说明，`WithoutOffset` 那边用 `From` 做了穷尽断言 |
| 请求体不是合法 JSON（axum） | axum 的 `Json` extractor 在进 handler 前返回 400 + 纯文本，不经过 `AxumHttpError`，不是 `{code,msg,data}`。测试按实际行为断言，是否统一待定 |
| `Resources::new()` 是 `pub(super)` | 组件 crate 的测试拿不到 `Resources`，只能先跑一个空 Registry 从回调里取（hygiea-http / grpc 的测试里有这段绕路代码）。是否公开构造函数待定 |
| 疑似 bug：`Registry::shutdown` | 组件有后台任务且等它结束超时时，这个组件的 `stop()` 不会被调用（"等任务结束再 stop"被打包成一个整体，外层超时直接打断），它自己的收尾逻辑不会执行。测试 `slow_stop_is_aborted_and_next_component_still_stops` 按现状断言，实现没改 |
| test-support `header_map` | 修了一个 bug：文档写"同名后者覆盖前者"，实际是追加成多值 |
| test-support `Reply` | 除了计划里的 `bytes`，为 read_timeout 和"连接被重置"两个测试加了 `body_delay`、`reset` |
| trybuild 拆成 fail / pass 两个函数 | trybuild 没有 pass 用例时只做 `cargo check`，`err!` 的 `const assert` 检查不会触发，fail 那组里挂了一个 pass 用例强制走 build，文件里有注释 |
