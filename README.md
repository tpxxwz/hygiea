# Hygiea

> 基于组件的 Rust 应用框架，附带错误、日志、日期、字符串等基础功能的封装和扩展

[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可证)

## 概述

`hygiea` 的核心是应用框架（`hygiea::app`）：把应用拆成组件（数据库、Redis、HTTP 服务、gRPC 服务……）注册到 `Registry`，由它负责加载配置、按依赖顺序启动组件、在组件之间共享资源、收到退出信号后按逆序关闭。常用的连接池和服务组件已经做好，开 feature 就能用。

围绕应用框架，还提供错误、日志、日期、字符串等基础功能的封装和扩展，这些也可以脱离应用框架单独使用。

使用方只依赖 facade crate `hygiea`，按需开启 feature。

## 功能

### 应用框架（`app` feature）

| 能力 | 说明 |
|---|---|
| 组件注册 | 在 `impl ImmediateComponent`（立即启动）或 `impl DeferredComponent`（延迟启动）上标 `#[component]`，`Registry::add` / `add_named` 注册；同一种组件可以按名字注册多个实例（比如主库和从库） |
| 两阶段启动 | 第一阶段启动所有组件（连接池连上、HTTP / gRPC 只绑端口），然后调用 `before_activate` 的回调（建表、初始化全局状态），第二阶段 Deferred 组件才开始对外服务，最后调用 `on_ready` 的回调（注册中心、已就绪打点）；Deferred 组件第一阶段在类型上就读不到资源 |
| 依赖排序 | 组件声明 `provides` / `depends_on`，Registry 做拓扑排序决定第一阶段的启动顺序，跟 `add` 的顺序无关；缺依赖、依赖成环、重复提供都在启动任何组件之前报错 |
| 资源共享 | 先启动的组件把资源放进 `Resources`，后启动的组件和业务代码按类型和名字取出来 |
| 配置加载 | 环境配置文件 → 额外配置文件 → 环境变量 → 命令行，后面的覆盖前面的；环境用 `--env` 或 `HYGIEA_ENV` 选 |
| 优雅关闭 | 收到 Ctrl+C / SIGTERM 后先关 Deferred 组件（停止接请求），再按启动的逆序关其余组件，每个组件单独计时，超时强制结束；支持关闭前延迟，方便 k8s 摘流量 |
| 日志 | 启动时按配置初始化 tracing（控制台、按时间滚动的文件） |
| 现成组件 | 数据库连接池（sqlx / SeaORM × PostgreSQL / SQLite）、Redis 连接池、HTTP 服务（axum）、gRPC 服务（tonic），见下文「组件」 |

### 基础能力（常开）

以下功能不需要开 feature。

| 模块 | 提供什么 |
|---|---|
| 错误处理（`hygiea::{HyErr, err!, bail!, hy_err}`） | 基于模板的错误信息，编译期校验的 8 位错误码，统一的 `HyErr` 类型，用 `with_source` 挂外部错误 |
| 日期时间（`hygiea::datetime`） | 基于 `time` crate 的 UTC 工具：格式化、解析、日 / 周 / 月 / 年的起止时刻 |
| 环境变量（`hygiea::env`） | 环境变量读取 |
| 字符串（`hygiea::string`） | 带缓存的正则，模板渲染（`fmt_tpl!`、`fmt_tpl_once!`） |

### 其他可选 feature

| Feature | 开启什么 |
|---|---|
| `app` | 应用框架，见上文（会带上 `log`） |
| `log` | tracing 初始化：控制台和按时间滚动的文件输出、过滤，可以脱离 `app` 单独用（会带上 `datetime-iana`） |
| `redact` | 日志打码：`#[redact]` 属性，标了 `#[redact(mask)]` / `#[redact(skip)]` 的字段只在序列化进日志时打码（任意嵌套层级都生效），正常序列化不受影响 |
| `datetime-iana` | IANA 时区和系统本地时间（`*_local` 方法） |
| `datetime-chrono` | 和 chrono 互相转换 |
| `ws-client` | WebSocket 客户端（开发中，目前为空） |
| `json` | 预留，目前为空 |
| `full` | 以上全部（不含组件） |

### 组件

可选的应用组件放在 `hygiea-components/` 下，每种能力一个 crate，用哪个框架或数据库由 feature 决定。通过 `hygiea` 的 feature 开启（都会带上 `app`）：

| facade feature | crate | 提供 |
|---|---|---|
| `db-sqlx-postgres` | `hygiea-db`（`sqlx` + `postgres`） | `hygiea::db::SqlxPgComponent` / `SqlxPgPool` |
| `db-sqlx-sqlite` | `hygiea-db`（`sqlx` + `sqlite`） | `hygiea::db::SqlxSqliteComponent` / `SqlxSqlitePool` |
| `db-seaorm-postgres` | `hygiea-db`（`seaorm` + `postgres`） | `hygiea::db::SeaOrmPgComponent` / `SeaOrmPgPool` |
| `redis-fred` | `hygiea-redis`（`fred`） | `hygiea::redis::RedisComponent` / `FredRedisPool` |
| `http-axum` | `hygiea-http`（`axum`） | `hygiea::http::AxumComponent` |
| `http-client-reqwest` | `hygiea-http-client`（`reqwest`） | `hygiea::http_client::reqwest_client::ReqwestComponent` / `ReqwestConfig`，请求 API 也在这个模块；在 `Resources` 中提供 `Client` |
| `grpc-tonic` | `hygiea-grpc`（`tonic`） | `hygiea::grpc::TonicComponent` |

## 快速开始

### 安装

在 `Cargo.toml` 中添加：

```toml
[dependencies]
# 错误处理、日期时间、环境变量、字符串不需要 feature
hygiea = "0.1.1-alpha.5"

# 应用框架，加上需要的组件
hygiea = { version = "0.1.1-alpha.5", features = ["app", "db-sqlx-postgres", "http-axum"] }

# 全部
hygiea = { version = "0.1.1-alpha.5", features = ["full"] }
```

或者用 `cargo add`：

```bash
cargo add hygiea@0.1.1-alpha.5 --features app,db-sqlx-postgres,http-axum
```

### 应用框架

用现成的 SQLite 和 HTTP 组件，配置从 `config/dev.toml` 自动加载。需要开 `app`、`db-sqlx-sqlite`、`http-axum`，另外要加 `axum`、`serde`（`derive`）、`tokio` 依赖。

`config/dev.toml`：

```toml
# 框架的日志配置（RegistryConfig.tracing）
[registry.tracing]
root_env_filter = "info"

# SqlxSqliteComponent 的配置（SqlxSqliteConfig）
[db]
database = ":memory:"   # 数据库文件路径；":memory:" 是内存库，不需要装数据库服务

# AxumComponent 的配置（AxumConfig）
[http]
port = 8080   # 不写 host 时监听 0.0.0.0
```

`src/main.rs`：

```rust
use axum::Router;
use axum::routing::get;
use hygiea::app::{ConfigArgs, IntoRegistryConfig, Registry, RegistryConfig};
use hygiea::db::{SqlxSqliteComponent, SqlxSqliteConfig};
use hygiea::http::{AxumComponent, AxumConfig};
use hygiea::HyErr;
use serde::Deserialize;

/// 对应 config/dev.toml：框架的配置在 [registry] 段，每个组件的配置各占一段
#[derive(Deserialize)]
struct AppConfig {
    registry: RegistryConfig,
    db: SqlxSqliteConfig,
    http: AxumConfig,
}

impl IntoRegistryConfig for AppConfig {
    fn registry_config(&self) -> RegistryConfig {
        self.registry.clone()
    }
}

#[tokio::main]
async fn main() -> Result<(), HyErr> {
    // 读 config/<env>.toml（默认 dev），再叠加 -f 指定的文件、HYGIEA__ 环境变量、--set
    let (registry, mut config) = Registry::load_config::<AppConfig>(&ConfigArgs::from_cli());
    config.http.router = Some(Router::new().route("/hello", get(|| async { "hello\n" })));

    let (result, _log_guard) = registry
        .add::<SqlxSqliteComponent>(config.db)
        .add::<AxumComponent>(config.http)
        .run()
        .await;
    result
}
```

- 环境名默认 `dev`，用 `--env prod` 或 `HYGIEA_ENV=prod` 切换到 `config/prod.toml`；`-f local` 叠加 `config/local.toml`，`HYGIEA__HTTP__PORT=9090` 或 `--set http.port=9090` 覆盖单个配置项。配置项的层级就是 `AppConfig` 的字段层级，比如日志级别是 `registry.tracing.root_env_filter`。
- 启动分两个阶段：先连上 SQLite、HTTP 绑好端口，然后 HTTP 才开始接请求；要在接请求之前做的初始化放进 `before_activate`（见 `app_two_phase`）。
- 按 Ctrl+C 退出时，先停 HTTP（处理完手上的请求），再按启动的逆序关连接池。

- 框架和所有组件的全部配置项、默认值和说明见 [docs/config-template.toml](docs/config-template.toml)，复制过去删掉用不到的段、改需要改的值即可。
- 配置目录默认是相对当前目录的 `config/`，可以用命令行 `-d <目录>`（`--config-dir`）或代码里 `ConfigArgs::from_cli().default_config_dir(..)` 换，命令行优先。

这段就是 `app_basic` 示例（示例里用 `default_config_dir` 指向 `hygiea-examples/config/app_basic/`）。自己实现组件、依赖排序、多实例、全局状态、后台任务、配置加载、两阶段启动各有一个单独的示例，见下文「示例」。

### 错误处理

```rust
use hygiea::{err, hy_err};

// 项目前缀 "001" 来自 Cargo.toml，见下文「错误码体系」
#[derive(hy_err)]
#[err_code_module_prefix = "01"]
pub enum UserErrors {
    #[error(err_code = "001", err_tpl = "User {{ name }} not found")]
    UserNotFound,

    #[error(err_code = "002", err_tpl = "Invalid email: {{ email }} ({{ reason }})")]
    InvalidEmail,

    // 固定信息就是不带变量的模板
    #[error(err_code = "003", err_tpl = "Avatar upload failed")]
    AvatarUploadFailed,
}

fn main() {
    // 一个变量：直接传值
    let e = err!(UserErrors::UserNotFound, "Alice");
    println!("{e} [{}]", e.err_code()); // User Alice not found [00101001]

    // 多个变量：逐个写名字
    let e = err!(UserErrors::InvalidEmail, { "email": "a@b", "reason": "no domain" });
    println!("{e}"); // Invalid email: a@b (no domain)

    // 没有变量：err!(X)。写法和模板对不上会编译报错
    // 底层原因用 with_source 挂上
    let e = err!(UserErrors::AvatarUploadFailed)
        .with_source(std::io::Error::other("storage unavailable"));
    println!("{e}");   // Avatar upload failed
    println!("{e:#}"); // Avatar upload failed: storage unavailable
    assert!(e.is(UserErrors::AvatarUploadFailed));
}
```

`Display` 只输出模板渲染结果，这是给客户端看的；`{:#}` 会在后面接上 `source` 链，用于服务端日志。

### HTTP 与日志打码

需要 `http-client-reqwest` feature。每次请求在 INFO 级别打 `http call start` / `http call success`，失败打 WARN（可配置）。标了 `#[redact(mask)]` 的字段在这些日志里显示为 `"***"`（包括 params、请求体、URL，以及 `send` 解码出的响应体），实际发送和接收的仍是真实值。非 2xx 和解码失败的响应原样记录，因为排查问题需要看原文。

```rust
use hygiea::HyErr;
use hygiea::http_client::reqwest_client::{Client, ReqwestConfig, Json, Method, RequestConfig};
use hygiea::redact::redact;
use serde::{Deserialize, Serialize};

// #[redact] 必须写在 #[derive(Serialize)] 上面
#[redact]
#[derive(Serialize)]
struct LoginReq {
    user: String,
    #[redact(mask)]
    password: String,
}

#[redact]
#[derive(Serialize, Deserialize)]
struct LoginResp {
    #[redact(mask)]
    token: String,
}

async fn login(client: &Client) -> Result<String, HyErr> {
    let req = LoginReq { user: "alice".into(), password: "p@ss".into() };
    // 请求体 Json(..) 表示按 JSON 编码；send::<Json<LoginResp>> 表示按 JSON 解码，resp.body 是 LoginResp
    let resp = RequestConfig::with_body(Method::POST, "https://api.example.com/login", Json(req))
        .send::<Json<LoginResp>>(client)
        .await?;
    Ok(resp.body.token)
}

// client 只建一次并共享：连接池在它里面
// let client = ReqwestConfig::default().build()?;
```

`send::<Decoder>` 的 `Decoder` 是解码器：`Json<T>` 解出 `T`，`String` / `Bytes` / `()` / `BodyStream` 解出它们自己。`resp.body` 的类型是解码器的输出，所以解码器要写在 turbofish 里，不能靠接收处的类型标注推断。自定义解码（拆 `{code, msg, data}` 外壳、解密……）实现 `FromBytes`，`Output` 可以是拆出来的业务数据。

请求发出去之后失败（传输失败、非 2xx、响应解码失败）时，错误的 err_args 统一带 `method`、`url`、`status`（还没收到响应时是 `null`）；读完了 body 的，还带 `body` 原文，不渲染进对外消息。

#### 重试

`send` 的参数换成 `(&client, RetryCtx::new(次数, 策略))` 就带重试：没拿到解码成功的响应时，把这次的配置和失败信息（`SendFailure`：错误，以及传输失败的超时 / 建连标记，或者原始的 status、headers、body）交给策略。策略可以按这些信息改配置，返回 `RetryDecision`：

- `Retry(cfg)`：确认可以重试，用这个配置再发；
- `Stop(err)`：立即结束，不管还剩几次；
- `Unknown(cfg)`：判断不了，原样再发，直到次数用完。

`Retry` 和 `Unknown` 共用同一个次数上限。发出之前的错误（URL、params、header）不重试，直接返回。

**只在两种情况下重试**：对方接口幂等（GET / PUT / DELETE，或者带了幂等键），或者根据错误调整请求后再发（401 换 token、签名过期重签……）。超时、读 body 中断时请求可能已经被对方执行了，不幂等的请求（下单、转账）不要原样重发。

重试要求请求体整个在内存里（`Json`、`Form`、`Raw`、`Multipart`，也就是实现了 `Clone` 的），解码器实现 `FromBytes`（body 读完再解码）；流式的 `RawStream`、reqwest 原生的 `multipart::Form`、`BodyStream` 开不了重试，编译期报错。

```rust
use std::time::Duration;
use hygiea::http_client::reqwest_client::{Auth, FailStage, RequestConfig, RetryCtx, RetryDecision, SendFailure};

type Cfg = RequestConfig<(), ()>;

let resp = RequestConfig::plain(Method::GET, url)
    .auth(Auth::Bearer(token))
    .send::<Json<Profile>>((&client, RetryCtx::new(3, |attempt: usize, cfg: Cfg, f: SendFailure| async move {
        match &f.stage {
            // 401：刷新 token 后再发
            FailStage::Status(resp) if resp.status == 401 => match refresh_token().await {
                Ok(token) => RetryDecision::Retry(cfg.auth(Auth::Bearer(token))),
                Err(e) => RetryDecision::Stop(e),
            },
            // 4xx 是请求本身的问题，重试也没用
            FailStage::Status(resp) if resp.status.is_client_error() => RetryDecision::Stop(f.err),
            // 其他（超时、5xx……）：GET 幂等，退避后原样再发，直到次数用完
            _ => {
                tokio::time::sleep(Duration::from_millis(100 << attempt)).await;
                RetryDecision::Unknown(cfg)
            }
        }
    })))
    .await?;
```

## 架构

采用 facade 模式。使用方只依赖 `hygiea` 并选择 feature，其余 crate 都是内部实现。

```
hygiea                         facade：重新导出全部内容，按 feature 开关
├── hygiea-core                核心实现
├── hygiea-macros              过程宏
└── hygiea-components/*        可选的应用组件，基于 hygiea-core 的应用框架
```

| crate | 发布 | 职责 |
|---|---|---|
| `hygiea` | 是 | 使用方唯一需要依赖的 crate。重新导出 core 和各组件，由 feature 决定编译哪些 |
| `hygiea-core` | 是 | 应用框架；错误处理、日志、打码、日期时间、环境变量、字符串、WebSocket 客户端 |
| `hygiea-macros` | 是 | `#[derive(hy_err)]` 和 `#[redact]` |
| `hygiea-db` | 是 | 数据库连接池组件（PostgreSQL、SQLite） |
| `hygiea-redis` | 是 | Redis 连接池组件 |
| `hygiea-http` | 是 | HTTP 服务组件 |
| `hygiea-http-client` | 是 | reqwest HTTP 客户端及组件 |
| `hygiea-grpc` | 是 | gRPC 服务组件 |
| `hygiea-test` | 是 | 给使用方写测试用，作为 dev-dependency：测试日志、mock HTTP 服务（httpmock + TOML cassette + Rhai）、测试用 http client |
| `test-support` | 否 | hygiea 自己各 crate 的测试共用工具，只作为 dev-dependency |
| `hygiea-examples` | 否 | 可运行的使用示例 |
| `playground` | 否 | 临时调查和暂存代码 |

各 crate 的模块说明，以及测试和示例放在哪：[docs/architecture.md](docs/architecture.md)。

使用方不需要额外依赖（比如 `linkme`、`serde_json`）。宏生成的代码跟随你依赖时用的名字：依赖 `hygiea`、给它改名、或者只依赖 `hygiea-core` 都可以。需要覆盖时用 `#[hy_err(crate = "path")]` / `#[redact(crate = "path")]`。

## 错误码体系

错误码固定 **8 位**，最多分三级：

| 级别 | 位数 | 写在哪 | 不写时 |
|---|---|---|---|
| 项目前缀 | 3 | Cargo.toml metadata 里的 `err_code_project_prefix` | `000` |
| 模块前缀 | 2 | enum 上的 `#[err_code_module_prefix = ".."]` | 没有模块这一级 |
| 错误编号 | 有模块前缀时 3 位，没有时 5 位 | 变体上的 `err_code` | 必填 |

项目前缀先查 crate 的 `[package.metadata.hygiea]`，再查 workspace 根的 `[workspace.metadata.hygiea]`，都没有就是 `000`。它不能写在 enum 上，所以一个项目里的所有 enum 共用同一个项目前缀。

```toml
# workspace 根的 Cargo.toml（单个 crate 可以在 [package.metadata.hygiea] 里覆盖）
[workspace.metadata.hygiea]
err_code_project_prefix = "001"
```

```rust
#[derive(hy_err)]
#[err_code_module_prefix = "01"]  // ← 模块前缀
pub enum UserErrors {
    #[error(err_code = "001", err_tpl = "...")]  // ← 错误编号，3 位
    //  最终错误码：001 01 001 = 00101001
    UserNotFound,
}

#[derive(hy_err)]                 // 没有模块前缀
pub enum LegacyErrors {
    #[error(err_code = "00001", err_tpl = "...")]  // ← 5 位
    //  最终错误码：001 00001 = 00100001
    Old,
}
```

### 保留和内置错误码

- `00000000` 保留给成功（`SUCCESS_CODE`），使用会编译报错。
- 项目前缀 `999` 给框架内置错误用。`BaseErr`（不需要 feature 的模块）没有模块前缀，用 5 位编号，兜底的 `SysErr` 是 `99999`；`hygiea::http_client::reqwest_client::BaseHttpErr`（`http-client-reqwest` feature）的模块前缀是 `01`。同一模块内重复会编译报错；跨模块或跨 crate 的重复在启动时检查（进程打印重复的错误码后以状态码 1 退出）。
- HTTP 组件（`hygiea::http`）的错误响应：状态码一律 200，错误放在 body 的 `code` / `msg`。业务错误原样返回模板渲染出的消息；`999` 开头的框架内置错误模板参数里可能有内部信息，对外统一换成 `SysErr`（"System Error"），原错误只记在服务端日志里。输入校验这类要给用户看的错误，用项目自己前缀的错误码定义。

| 错误码 | 变体 | 模板 |
|---|---|---|
| `99900001` | `BaseErr::DateError` | Date error: {{ cause }} |
| `99900002` | `BaseErr::RegexError` | Invalid regex: {{ pattern }} |
| `99900003` | `BaseErr::JsonError` | JSON error: {{ cause }} |
| `99900004` | `BaseErr::TemplateError` | Template error: {{ cause }} |
| `99900005` | `BaseErr::EnvError` | Environment variable not set: {{ name }} |
| `99901001` | `BaseHttpErr::ClientBuildFailed` | Http client build failed |
| `99901101` | `BaseHttpErr::InvalidUrl` | Invalid url: {{ url }} |
| `99901102` | `BaseHttpErr::InvalidParams` | Invalid query params: {{ method }} {{ url }} |
| `99901103` | `BaseHttpErr::InvalidHeader` | Invalid header: {{ cause }} |
| `99901104` | `BaseHttpErr::RequestBuildFailed` | Http request build failed: {{ method }} {{ url }} |
| `99901201` | `BaseHttpErr::RequestFailed` | Http request failed: {{ method }} {{ url }} |
| `99901202` | `BaseHttpErr::NonSuccessStatus` | Http {{ status }}: {{ method }} {{ url }} |
| `99901203` | `BaseHttpErr::WriteFailed` | Write response body failed |
| `99901204` | `BaseHttpErr::DecodeFailed` | Http response decode failed: {{ method }} {{ url }} |
| `99999999` | `BaseErr::SysErr` | System Error |

## 示例

示例在 `hygiea-examples/examples/` 下，按主题分目录，每个示例只讲一件事，都不需要装数据库等外部服务：

| 示例 | 讲什么 |
|---|---|
| `app_basic` | 最小的应用：现成的 SQLite 和 HTTP 组件，配置从 `config/app_basic/dev.toml` 加载 |
| `app_dependencies` | 自己实现组件；`provides` / `depends_on` 决定启动顺序，缺依赖时启动前就报错 |
| `app_named_instances` | 同一种组件注册多个实例（主库、从库），按名字取 |
| `app_global_state` | 启动后把资源收进全局 `AppState`，业务代码直接取 |
| `app_two_phase` | 两阶段启动：`before_activate` 在 HTTP 开始接请求之前建表、初始化全局 `AppState`，handler 从 `AppState` 取连接池 |
| `app_background_task` | 后台任务、优雅关闭、超时强制结束、`stop` 收尾 |
| `app_config` | 配置文件、`-f`、环境变量、`--set` 的合并，框架参数合进自己的命令行 |
| `error_basic` | 错误 enum、`err!` 的写法、错误码 |
| `string_template` | `fmt_tpl!`、`fmt_tpl_once!`、`fmt_pos!` |
| `log_basic` | 控制台 + 文件双输出、按 layer 设置 filter、日志时间用 UTC |
| `datetime_basic` | 格式化和解析、日 / 周 / 月边界、系统本地时区 |
| `env_basic` | `BuiltinKey`、自定义 `EnvKey`、`env_get` / `env_get_opt` / `env_get_or` / `env_get_or_else` |
| `redact_basic` | `#[redact(mask/skip)]`、`to_redacted_json`，对比普通 `serde_json` 序列化 |
| `http_client_basic` | 请求本进程里起的 axum 服务，演示打码后的日志 |
| `http_axum_response` | `AxumHttpResponse` / `AxumHttpError`、请求体大小限制 |

```bash
cargo run -p hygiea-examples --example app_basic
```

app 的示例启动后按 Ctrl+C 退出，可以看到组件的关闭顺序：先关 HTTP 这类 Deferred 组件，再按启动的逆序关其余组件。

## 开发

### 构建

```bash
# 构建所有 crate
cargo build

# 开启全部 feature 构建
cargo build --all-features

# 跑全部测试，包括 hygiea-macros/tests 里的编译失败测试（trybuild）
cargo test --workspace --all-features

# 改了宏的报错信息后，重新生成 trybuild 快照并检查 diff
TRYBUILD=overwrite cargo test -p hygiea-macros --test hy_err_ui --test redact_ui

# 查看文档
cargo doc --open
```

## 设计原则

1. **Facade 模式**：使用方只接触 `hygiea`，内部结构不暴露
2. **按 feature 编译**：只编译用到的部分
3. **零成本**：抽象在编译期消除，没有运行时开销
4. **类型安全**：借助 Rust 类型系统保证正确性
5. **好用**：容易用对，不容易用错

## 许可证

可任选 Apache License 2.0 或 MIT License。

## 联系方式

作者：wjj (tpxxwz)
邮箱：tpxxwz@gmail.com
GitHub：[@tpxxwz](https://github.com/tpxxwz)

## 致谢

本项目参考了：

- [serde](https://github.com/serde-rs/serde)：facade 模式和 workspace 组织方式
- [thiserror](https://github.com/dtolnay/thiserror)：过程宏架构
