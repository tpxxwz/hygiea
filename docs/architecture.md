# 架构

各 crate 的模块和能力、代码和测试放在哪。

## 总原则

- 使用方只依赖 `hygiea`，按 feature 开启功能，路径一律写 `hygiea::xxx`。其余 crate 都是内部实现，不直接给使用方依赖。
- 过程宏生成的代码引用 `::hygiea::` 路径，所以宏的测试放在 `hygiea` 里做。

## 各 crate 详细说明

crate 的分层和一句话职责见根目录 `README.md` 的 Architecture 一节，这里展开说明。

### hygiea

| 文件 | 内容 |
|---|---|
| `src/lib.rs` | `pub use hygiea_core::*;` 导出 core 的全部内容，再按 feature 导出各组件（core 不能依赖组件，所以组件只能由 facade 导出） |
| `Cargo.toml` | core 的 feature 一对一转发，feature 之间的依赖关系只在 core 里写；组件 feature 引入对应的组件 crate |
| `tests/config_template.rs` | 按各配置结构体解析 `docs/config-template.toml`，并确认模板里的值等于代码默认值（需要 `--all-features`） |

### hygiea-core 的模块

| 模块 | feature | 内容 |
|---|---|---|
| `error` | 常开 | `HyErr`、`err!` / `bail!`、错误码 |
| `redact` | `redact`（`http-client-reqwest` 会带上） | 日志打码时的序列化支持 |
| `datetime` | 常开；`datetime-iana` 开本地时区，`datetime-chrono` 开 chrono 互转 | 基于 `time` 的格式化、解析、日界计算 |
| `env` | 常开 | 环境变量 |
| `string` | 常开 | 缓存的正则、模板渲染，`fmt_tpl!` / `fmt_tpl_once!` / `fmt_pos!` 宏 |
| `log` | `log` | 日志配置和初始化 |
| `app` | `app` | 应用框架：组件（`Component` 加上 `ImmediateComponent` / `DeferredComponent` 两种启动方式）、注册、依赖排序、两阶段启动（`before_activate` / `on_ready`）、优雅退出；`#[component]` 宏从这里导出 |
| `net` | `ws-client` | WebSocket 客户端 |

### hygiea-macros

| 文件 | 内容 |
|---|---|
| `src/error.rs` | `#[derive(hy_err)]`：校验错误码和模板，生成错误码注册 |
| `src/redact.rs` | `#[redact]`：字段打码、跳过。`redact` feature 开了才编译 |
| `src/component.rs` | `#[component]`：把 `impl ImmediateComponent` / `impl DeferredComponent` 块拆成 `impl Component` 和启动 trait 两个 impl，按 trait 填 `Kind`。`component` feature（core 的 `app` 会打开）开了才编译 |
| `src/krate.rs` | 按调用方的依赖名决定生成代码里用 `::hygiea` 还是 `::hygiea_core`，`crate = "path"` 可以覆盖 |
| `tests/` | trybuild 测试：`hy_err_ui`（`#[derive(hy_err)]`、`err!`）、`redact_ui`（`#[redact]`）、`component_ui`（`#[component]`，包括 Deferred 组件第一阶段读不到资源）。dev-dependency 依赖 facade，fixture 用 `hygiea::` 路径，和下游写法一样 |

### 组件

组件按能力划分 crate，具体用哪个框架或数据库由 feature 决定。组件都依赖 core 的 `app`。

| crate | 框架 feature | 其他 feature | facade 的 feature | facade 的路径 |
|---|---|---|---|---|
| `hygiea-db` | `sqlx`、`seaorm` | `postgres`、`sqlite` | `db-sqlx-postgres`、`db-sqlx-sqlite`、`db-seaorm-postgres` | `hygiea::db` |
| `hygiea-redis` | `fred` | | `redis-fred` | `hygiea::redis` |
| `hygiea-http` | `axum` | | `http-axum` | `hygiea::http` |
| `hygiea-http-client` | `reqwest` | | `http-client-reqwest` | `hygiea::http_client`（实现在 `reqwest_client` 子模块） |
| `hygiea-grpc` | `tonic` | | `grpc-tonic` | `hygiea::grpc` |

| 文件 | 内容 |
|---|---|
| `hygiea-db/src/sqlx_postgres.rs`、`sqlx_sqlite.rs`、`seaorm_postgres.rs` | 各框架与数据库组合的连接池组件，以及对应的锁实现（PostgreSQL 用 advisory lock，SQLite 用进程内锁） |
| `hygiea-db/src/pg_advisory.rs` | 字符串 key 转 advisory lock 的 i64 key，sqlx 和 SeaORM 共用 |
| `hygiea-redis/src/fred_pool.rs` | 连接池组件；锁实现为 SET NX + token + 看门狗续期，释放用 Lua |
| `hygiea-http/src/axum_server.rs` | HTTP 服务组件 |
| `hygiea-http-client/src/lib.rs` | 按 feature 声明各实现的模块，不在 crate 根上再导出（以后加别的实现时名字不撞） |
| `hygiea-http-client/src/reqwest_client/mod.rs` | 模块结构说明和再导出，不放实现 |
| `hygiea-http-client/src/reqwest_client/client.rs` | `ReqwestConfig`（client 级配置）和 `ReqwestComponent`；`config.rs` 是配置文件里用的请求头、代理类型 |
| `hygiea-http-client/src/reqwest_client/request.rs` | `RequestConfig`（单次请求，纯数据）和 `HttpResponse`；`headers.rs` 请求头与认证，`body.rs` 请求体，`response.rs` 响应体 |
| `hygiea-http-client/src/reqwest_client/send.rs` | 发送流程（开头有流程图）；`retry.rs` 重试；`logging.rs` 日志；`text.rs` 字节转文本；`error.rs` 错误 |
| `hygiea-grpc/src/tonic_server.rs` | gRPC 服务组件 |

### hygiea-test-support

| 模块 | 内容 |
|---|---|
| 根 | `Unserializable` 等通用小工具 |
| `headers` | 构造请求头 |
| `logs` | 捕获 tracing 输出，断言日志内容 |
| `http_server` | 本地 HTTP 服务，给 HTTP 客户端的端到端测试用 |

### hygiea-examples

示例按主题分目录，每个示例一个文件、只讲一件事：

| 目录 | 示例 |
|---|---|
| `examples/app/` | `app_basic`、`app_dependencies`、`app_named_instances`、`app_global_state`、`app_background_task`、`app_config`、`app_two_phase` |
| `examples/error/` | `error_basic` |
| `examples/string/` | `string_template` |
| `config/<示例名>/` | 读配置文件的示例各用一个目录（`ConfigArgs::default_config_dir` 指定）：`app_basic/dev.toml`；`app_config/` 下 `dev.toml`、`prod.toml`、`local.toml` |

Cargo 只自动发现 `examples/*.rs` 和 `examples/*/main.rs`，所以每个示例都在 `Cargo.toml` 里用 `[[example]]` 声明，名字用 `<目录>_<文件>`。新加示例时照这个格式加一段。示例不连外部服务，数据库用 SQLite 内存库。

## 测试和示例放哪

| 内容 | 位置 |
|---|---|
| 单元测试（可以测私有函数） | 源文件里的 `#[cfg(test)] mod tests` |
| crate 的集成测试（只用本 crate 的 pub API） | `<crate>/tests/` |
| 宏的测试：编译期报错（trybuild），fixture 经 facade 使用宏（和下游写法一样） | `hygiea-macros/tests/` |
| 给使用方看的示例，`cargo run -p hygiea-examples --example xxx` | `hygiea-examples/examples/` |
| 调查、速查、暂存代码 | `hygiea-playground/tests/` |

补充约定：

- 组件的测试放在组件 crate 自己的 `tests/` 里，不放进 examples 或 playground。需要真实服务（PostgreSQL、Redis）的测试标 `#[ignore = "requires running ..."]`，并在文件头写明手动运行的命令。
- 需要联网的测试也标 `#[ignore]`。
- 除 `hygiea-examples` 以外，其他 crate 不建 `examples/`。
- `hygiea-playground` 是独立的 crate，不在 workspace 里（根 `Cargo.toml` 的 `exclude`），有自己的 `Cargo.lock` 和 `target/`，要在它的目录里执行 `cargo test`。只有 `tests/`。`src/lib.rs` 是空的，留着只是因为 Cargo 要求每个包至少有一个 lib 或 bin。playground 里的代码不保证一直能编译通过。
- `hygiea-test-support` 不单独建 `tests/`。它会在各 crate 的测试里被用到，出问题那些测试就会失败。里面有独立逻辑时，在对应文件里写单元测试。
