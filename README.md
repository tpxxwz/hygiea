# Hygiea

> 基于组件的 Rust 应用框架，附带错误、日志、日期、字符串等基础功能的封装和扩展

[![Rust](https://img.shields.io/badge/rust-1.94.1%2B-orange.svg)](https://www.rust-lang.org/)
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
| 两阶段启动 | 第一阶段启动所有组件（连接池连上、HTTP / gRPC 只绑端口），然后调用 `before_activate` 的回调（建表、组装应用状态放进 Resources），第二阶段 Deferred 组件才开始对外服务（HTTP / gRPC 这时才调用建 router 的函数，能取到前面放进去的状态），最后调用 `on_ready` 的回调（注册中心、已就绪打点）；Deferred 组件第一阶段在类型上就读不到资源 |
| 依赖排序 | 组件声明 `provides` / `depends_on`，Registry 做拓扑排序决定第一阶段的启动顺序，跟 `add` 的顺序无关；缺依赖、依赖成环、重复提供都在启动任何组件之前报错 |
| 资源共享 | 先启动的组件把资源放进 `Resources`，后启动的组件和业务代码按类型和名字取出来 |
| 配置加载 | 环境配置文件 → 额外配置文件 → 环境变量 → 命令行，后面的覆盖前面的；环境用 `--env` 或 `HYGIEA_ENV` 选 |
| 优雅关闭 | 收到 Ctrl+C / SIGTERM 后先关 Deferred 组件（停止接请求），再按启动的逆序关其余组件，每个组件单独计时，超时强制结束；支持关闭前延迟，方便 k8s 摘流量 |
| 日志 | 启动时按配置初始化 tracing（控制台、按时间滚动的文件） |
| 现成组件 | 数据库连接池（sqlx / SeaORM × PostgreSQL / MySQL / SQLite）、Redis 连接池、HTTP 服务（axum）、gRPC 服务（tonic）、AWS S3 客户端，见下文「组件」 |

### 基础能力（常开）

以下功能不需要开 feature。

| 模块 | 提供什么 |
|---|---|
| 错误处理（`hygiea::{HyErr, err!, bail!, hy_err}`） | 基于模板的错误信息，编译期校验的 8 位错误码，统一的 `HyErr` 类型，用 `with_source` 挂外部错误 |
| 日期时间（`hygiea::datetime`） | 基于 `time` crate：UTC 时刻和钟面时间 `Time` 的格式化、解析，日 / 周 / 月 / 年的起止时刻，单个值和时间段（`Range`）的 serde 读写模块 |
| 环境变量（`hygiea::env`） | 环境变量读取 |
| 字符串（`hygiea::string`） | 带缓存的正则，模板渲染（`fmt_tpl!`、`fmt_tpl_once!`），字符串固定地映射成整数（`xxh3_64` / `xxh32`，跨进程结果一样，不是解析数字） |

<!-- check:features -->
### 其他可选 feature

| Feature | 开启什么 |
|---|---|
| `app` | 应用框架，见上文（会带上 `log`） |
| `log` | tracing 初始化：控制台和按时间滚动的文件输出、过滤，可以脱离 `app` 单独用（会带上 `datetime-iana`） |
| `lock` | 分布式锁的 trait（`hygiea::lock::DistributedLock`）和 key 的注册（`#[derive(hy_lock_type)]`、`#[derive(hy_lock_key)]`、`lock_key!`），后端由组件实现（见 `db-lock`），见下文「分布式锁」 |
| `lock-tokio` | 进程内的锁 `hygiea::lock::TokioLock`：tokio 的 Mutex 实现 `DistributedLock`，不跨进程（会带上 `lock`） |
| `live-config` | 动态配置的 `#[derive(hy_live_config)]`（`hygiea::db::live_config::hy_live_config`），单开没用，经 `db-live-config` 打开，不在 `full` 里 |
| `redact` | 日志打码：`#[redact]` 属性，标了 `#[redact(mask)]` / `#[redact(skip)]` 的字段只在序列化进日志时打码（任意嵌套层级都生效），正常序列化不受影响 |
| `datetime-iana` | IANA 时区和系统本地时间（`*_local` 方法和 serde 读写模块） |
| `datetime-chrono` | 和 chrono 互相转换 |
| `ws-client` | WebSocket 客户端（开发中，目前为空） |
| `json` | 预留，目前为空 |
| `full` | 以上全部（不含组件） |

### 组件

可选的应用组件放在 `hygiea-components/` 下，每种能力一个 crate，用哪个框架或数据库由 feature 决定。通过 `hygiea` 的 feature 开启（都会带上 `app`）：

| facade feature | crate | 提供 |
|---|---|---|
| `db-sqlx` / `db-seaorm` | `hygiea-db`（`sqlx` / `seaorm`） | 选框架，和下面的数据库组合出连接池组件；开 `db-sqlx` 或 `db-seaorm` 就有 `hygiea::db::HyUtcDateTime` |
| `db-postgres` / `db-sqlite` / `db-mysql` | `hygiea-db`（`postgres` / `sqlite` / `mysql`） | 选数据库，对开了的框架都生效。MySQL 要 8.0 以上 |
| `db-live-config` | `hygiea-db`（`live-config`） | 数据库配置表（动态配置）：组件的 `uses` 带 2 时按组件名提供 `hygiea::db::DbLiveConfig`，改了表程序自动拿到新值，见下文「动态配置」 |
| `db-lock` | `hygiea-db`（`lock`），带上 `lock` | 分布式锁：PG、MySQL 组件的 `uses` 带 4 时匿名提供 `hygiea::db::DbLockClient`（实现 `DistributedLock`），见下文「分布式锁」 |
| `db-lock-row` | `hygiea-db`（`lock-row`），带上 `db-lock` | PG 的字符串 key 用锁表实现；不开时 PG 不支持字符串 key。和 MySQL 无关 |
| `redis-fred` | `hygiea-redis`（`fred`） | `hygiea::redis::RedisComponent` / `FredRedisPool` |
| `http-axum` | `hygiea-http`（`axum`） | `hygiea::http::AxumComponent` |
| `http-client-reqwest` | `hygiea-http-client`（`reqwest`） | `hygiea::http_client::reqwest_client`：`ReqwestComponent`，在 `Resources` 中提供 `ReqwestClient`，见下文「HTTP 客户端与日志打码」 |
| `http-client-reqwest-debug-log` | `hygiea-http-client`（`debug-log`，带上 `reqwest`） | 带原文和请求头、响应头的 debug 日志，只在 dev-dependencies 里开 |
| `grpc-tonic` | `hygiea-grpc`（`tonic`） | `hygiea::grpc::TonicComponent` |
| `aws-s3` | `hygiea-aws`（`s3`） | `hygiea::aws::AwsS3Component` / `AwsConfig`；在 `Resources` 中提供 `AwsS3Client`（`Deref` 到 `aws_sdk_s3::Client`） |

db 的三个维度各自独立、只增加功能，同时打开的组合才有东西，比如 `features = ["db-sqlx", "db-postgres", "db-lock"]`。支持的组合：

| 框架 | 数据库 | 组件 / 业务池 | `db-live-config` | `db-lock` / `db-lock-row` |
|---|---|---|---|---|
| `db-sqlx` | `db-postgres` | `SqlxPgComponent` / `SqlxPgPool` | 支持（NOTIFY） | 支持 |
| `db-sqlx` | `db-mysql` | `SqlxMySqlComponent` / `SqlxMySqlPool` | 支持（轮询） | 支持（`GET_LOCK`，`db-lock-row` 不影响） |
| `db-sqlx` | `db-sqlite` | `SqlxSqliteComponent` / `SqlxSqlitePool` | 支持（轮询） | — |
| `db-seaorm` | `db-postgres` | `SeaOrmPgComponent` / `SeaOrmPgPool` | 支持（NOTIFY） | 支持 |
| `db-seaorm` | `db-mysql` | `SeaOrmMySqlComponent` / `SeaOrmMySqlPool` | 支持（轮询） | 支持（`GET_LOCK`，`db-lock-row` 不影响） |
| `db-seaorm` | `db-sqlite` | `SeaOrmSqliteComponent` / `SeaOrmSqlitePool` | 支持（轮询） | — |
<!-- /check:features -->

6 个组件都是同一个泛型组件 `DbComponent` 的别名，提供什么由配置的 `uses` 决定，按位加：1 业务池、2 动态配置、4 锁，不写是 1。
配置分段、多个库怎么注册、不经过 Registry 怎么建，见 [`hygiea::db`](https://docs.rs/hygiea/latest/hygiea/db/index.html) 的 crate 文档。

## 快速开始

### 安装

在 `Cargo.toml` 中添加：

```toml
[dependencies]
# 错误处理、日期时间、环境变量、字符串不需要 feature
hygiea = "0.1.1-alpha.7"

# 应用框架，加上需要的组件
hygiea = { version = "0.1.1-alpha.7", features = ["app", "db-sqlx", "db-postgres", "http-axum"] }

# 全部
hygiea = { version = "0.1.1-alpha.7", features = ["full"] }
```

或者用 `cargo add`：

```bash
cargo add hygiea@0.1.1-alpha.7 --features app,db-sqlx,db-postgres,http-axum
```

### 应用框架

用现成的 SQLite 和 HTTP 组件，配置从 `config/dev.toml` 自动加载。需要开 `app`、`db-sqlx`、`db-sqlite`、`http-axum`，另外要加 `axum`、`serde`（`derive`）、`tokio` 依赖。

`config/dev.toml`：

```toml
# 框架的日志配置（RegistryConfig.tracing）
[registry.tracing]
root_env_filter = "info"

# SqlxSqliteComponent 的配置（SqlxSqliteConfig）：连哪个库在 [db.connect]，池的参数在 [db.pool]
[db.connect]
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
use hygiea::http::{AxumComponent, AxumComponentConfig, AxumConfig};
use hygiea::Result;
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
async fn main() -> Result<()> {
    // 读 config/<env>.toml（默认 dev），再叠加 -f 指定的文件、HYGIEA__ 环境变量、--set
    let (registry, config) = Registry::load_config::<AppConfig>(&ConfigArgs::from_cli());
    let http = AxumComponentConfig {
        config: config.http,
        router: Box::new(|_| Ok(Router::new().route("/hello", get(|| async { "hello\n" })))),
    };

    let (result, _log_guard) = registry
        .add::<SqlxSqliteComponent>(config.db)
        .add::<AxumComponent>(http)
        .run()
        .await;
    result
}
```

- 应用名是 `[registry] app_name`（小写下划线，默认 `hygiea`），环境变量前缀要是它的大写：自己的应用写 `app_name = "my_app"`，代码里配 `ConfigArgs::from_cli().env_prefix("MY_APP")`，对不上启动时报错。组件从 `Resources::registry_config()` 拿到它（`ResourceSink` / `ReadyResources` 上也有），`ConfigResource::from_config` 的第二个参数就是它，比如数据库连接的 `application_name`。
- 环境名默认 `dev`，用 `--env prod` 或 `HYGIEA_ENV=prod` 切换到 `config/prod.toml`；`-f local` 叠加 `config/local.toml`，`HYGIEA__HTTP__PORT=9090` 或 `--set http.port=9090` 覆盖单个配置项。配置项的层级就是 `AppConfig` 的字段层级，比如日志级别是 `registry.tracing.root_env_filter`。
- 启动分两个阶段：先连上 SQLite、HTTP 绑好端口，然后 HTTP 才开始接请求；要在接请求之前做的初始化放进 `before_activate`（见 `app_two_phase`）。
- 按 Ctrl+C 退出时，先停 HTTP（处理完手上的请求），再按启动的逆序关连接池。

- 框架和每个组件的全部配置项、默认值和说明，在各自 crate 的 `config/` 下（模板里生效的值就是代码默认值，可选项写成注释，互斥的写法写在同一份里、注释说明怎么切换），复制过去删掉用不到的项、改需要改的值即可；段名按自己 `AppConfig` 的字段名改。db 的模板例外的是 `uses`：模板写的是全开（列出全部段），代码默认是 1：

  | 配置 | 模板 |
  |---|---|
  | `RegistryConfig`（`[registry]`，框架和日志） | [hygiea-core/config/registry.toml](hygiea-core/config/registry.toml) |
  | `SqlxPgConfig`（`db-sqlx` + `db-postgres`） | [hygiea-components/hygiea-db/config/sqlx-pg.toml](hygiea-components/hygiea-db/config/sqlx-pg.toml) |
  | `SeaOrmPgConfig`（`db-seaorm` + `db-postgres`） | [hygiea-components/hygiea-db/config/seaorm-pg.toml](hygiea-components/hygiea-db/config/seaorm-pg.toml) |
  | `SqlxSqliteConfig`（`db-sqlx` + `db-sqlite`） | [hygiea-components/hygiea-db/config/sqlx-sqlite.toml](hygiea-components/hygiea-db/config/sqlx-sqlite.toml) |
  | `SeaOrmSqliteConfig`（`db-seaorm` + `db-sqlite`） | [hygiea-components/hygiea-db/config/seaorm-sqlite.toml](hygiea-components/hygiea-db/config/seaorm-sqlite.toml) |
  | `SqlxMySqlConfig`（`db-sqlx` + `db-mysql`） | [hygiea-components/hygiea-db/config/sqlx-mysql.toml](hygiea-components/hygiea-db/config/sqlx-mysql.toml) |
  | `SeaOrmMySqlConfig`（`db-seaorm` + `db-mysql`） | [hygiea-components/hygiea-db/config/seaorm-mysql.toml](hygiea-components/hygiea-db/config/seaorm-mysql.toml) |
  | `RedisConfig`（`redis-fred`） | [hygiea-components/hygiea-redis/config/redis.toml](hygiea-components/hygiea-redis/config/redis.toml) |
  | `AxumConfig`（`http-axum`） | [hygiea-components/hygiea-http/config/axum.toml](hygiea-components/hygiea-http/config/axum.toml) |
  | `ReqwestConfig`（`http-client-reqwest`） | [hygiea-components/hygiea-http-client/config/reqwest.toml](hygiea-components/hygiea-http-client/config/reqwest.toml) |
  | `TonicConfig`（`grpc-tonic`） | [hygiea-components/hygiea-grpc/config/tonic.toml](hygiea-components/hygiea-grpc/config/tonic.toml) |
  | `AwsConfig`（`aws-s3`） | [hygiea-components/hygiea-aws/config/aws.toml](hygiea-components/hygiea-aws/config/aws.toml) |

- 配置的通用规则：所有配置项都可以不写，不写用默认值；写了不存在的配置项（拼错也算）启动加载配置时报错，不会被静默忽略。
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

### 分布式锁

`lock` feature 提供 trait 和 key，后端由组件实现（`db-lock`：PG、MySQL 连接池组件提供的 `hygiea::db::DbLockClient`）；只要进程内互斥的用 `lock-tokio` 的 `TokioLock`。锁不可重入，拿不到返回 `Ok(None)`。
key 有三种，锁类型和字符串 key 都要注册，编号的规则和错误码一样（项目前缀 3 位 + 模块前缀 + 编号，共 9 位，和错误码共用项目前缀），
两个 derive 共用一套编号，同模块编译期、跨 crate 启动时查重：

```rust
use hygiea::lock::{DistributedLock, hy_lock_key, hy_lock_type, lock_key};

#[derive(hy_lock_type)]
#[lock_module_prefix = "02"]
pub enum OrderLockType {
    #[lock_type(code = "0001")]
    Pay,                                                   // 001 02 0001 = 001020001
}

#[derive(hy_lock_key)]
#[lock_module_prefix = "02"]
pub enum OrderLockKey {
    #[lock_key(code = "0002", tpl = "settle:{{ day }}")]
    Settle,                                                // key：001020002:settle:<day>
}

async fn run(db: &impl DistributedLock, order_id: u32, day: &str) -> hygiea::Result<()> {
    // 锁类型 + i32：PG 上是原生的 pg_try_advisory_xact_lock(int, int)
    if let Some(guard) = db.try_lock_i32(OrderLockType::Pay, order_id).await? {
        guard.release().await?;
    }
    // 任意 i64，不用注册：PG 上是 pg_try_advisory_xact_lock(bigint)
    let _ = db.try_lock_i64(42).await?;
    // 字符串 key：参数的写法和 err! 一样，对不上编译报错；阻塞版最多等 timeout
    let _ = db.lock(lock_key!(OrderLockKey::Settle, day), std::time::Duration::from_secs(3)).await?;
    Ok(())
}
```

编号规则、key 的限制、各后端的差异见 [`hygiea::lock`](https://docs.rs/hygiea/latest/hygiea/lock/index.html) 的模块文档，PG / MySQL 的实现见
[`hygiea::db::lock`](https://docs.rs/hygiea/latest/hygiea/db/lock/index.html)，设计说明见 [docs/design/lock.md](docs/design/lock.md)。

### 动态配置

需要 `db-live-config` 和一个数据库组件（比如 `db-sqlx` + `db-postgres`），组件的 `uses` 带 2：配置存在库里，改了表，运行中的程序自动拿到新值，
`#[derive(hy_live_config)]` 生成按字段读的句柄。用法见 [`hygiea::db::live_config`](https://docs.rs/hygiea/latest/hygiea/db/live_config/index.html) 的模块文档，
内部流程见 [docs/design/live-config.md](docs/design/live-config.md)。

### HTTP 客户端与日志打码

`http-client-reqwest` 提供 reqwest 客户端组件和请求 API：每次请求打一条开始、一条结束的日志，标了 `#[redact(mask)]` 的字段在日志里显示为 `"***"`，实际发送和接收的仍是真实值；可以带重试，测试里可以打开带原文的 debug 日志。
用法见 [`hygiea::http_client::reqwest_client`](https://docs.rs/hygiea/latest/hygiea/http_client/reqwest_client/index.html) 的模块文档。

## 架构

采用 facade 模式。使用方只依赖 `hygiea` 并选择 feature（写测试时另加 dev-dependency `hygiea-test`），其余 crate 都是内部实现。

```
hygiea                         facade：重新导出全部内容，按 feature 开关
├── hygiea-core                核心实现
├── hygiea-macros              过程宏
└── hygiea-components/*        可选的应用组件，基于 hygiea-core 的应用框架
```

| crate | 发布 | 职责 |
|---|---|---|
| `hygiea` | 是 | 使用方唯一需要依赖的 crate。重新导出 core 和各组件，由 feature 决定编译哪些 |
| `hygiea-core` | 是 | 应用框架；错误处理、日志、打码、分布式锁的 trait、日期时间、环境变量、字符串、WebSocket 客户端 |
| `hygiea-macros` | 是 | `#[derive(hy_err)]`、`#[derive(hy_lock_type)]` / `#[derive(hy_lock_key)]`、`#[component]`、`#[redact]` 和动态配置的 `#[derive(hy_live_config)]` |
| `hygiea-db` | 是 | 数据库组件（PostgreSQL、MySQL、SQLite，sqlx / SeaORM），按配置的 `uses` 提供业务池、数据库配置表（动态配置，[设计说明](docs/design/live-config.md)）、分布式锁（PG、MySQL，[设计说明](docs/design/lock.md)）；SeaORM 的错误转换（`db!`）、事务、分页。用法见 crate 文档 |
| `hygiea-redis` | 是 | Redis 连接池组件 |
| `hygiea-http` | 是 | HTTP 服务组件 |
| `hygiea-http-client` | 是 | reqwest HTTP 客户端及组件 |
| `hygiea-grpc` | 是 | gRPC 服务组件 |
| `hygiea-aws` | 是 | AWS SDK 组件（公共配置 + S3 客户端） |
| `hygiea-test` | 是 | 给使用方写测试用，作为 dev-dependency：测试日志、mock HTTP 服务（httpmock + TOML cassette + Rhai）、测试分层宏（`#[container]` / `#[live]`）、启动容器 |
| `hygiea-test-macros` | 是 | `hygiea-test` 的过程宏（`#[container]` / `#[live]`），经 `hygiea-test` 使用 |

使用方不需要额外依赖（比如 `linkme`、`serde_json`）。宏生成的代码跟随你依赖时用的名字：依赖 `hygiea`、给它改名、或者只依赖 `hygiea-core` 都可以。需要覆盖时用 `#[hy_err(crate = "path")]` / `#[hy_lock_type(crate = "path")]` / `#[hy_lock_key(crate = "path")]` / `#[redact(crate = "path")]` / `#[hy_live_config(crate = "path")]`（`hy_live_config` 写 facade 的路径，后面接 `::db::live_config`）。

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
- 项目前缀 `999` 给框架内置错误用，使用 hygiea 的项目不要用 `999`。
- 框架自己的 crate 模块多、单个模块错误少，所以 `999` 下用 3 位的 `#[err_code_internal_module_prefix = "..."]`，错误编号 2 位（`999` + `100` + `01` = `99910001`）。
  两种布局在同一个项目前缀下会撞码（`10`+`001` 和 `100`+`01` 都是 `10001`），所以宏强制：`999` 下只能用 3 位的，其他项目只能用 2 位的 `err_code_module_prefix`，写错编译报错。
  内部模块前缀按依赖层次分区间：

  | 区间 | 给谁 |
  |---|---|
  | `000`、`999` | `BaseErr`：不需要 feature 的模块，没有模块前缀，5 位编号。只用 `000xx`，兜底的 `SysErr` 是 `99999`；别的 5 位码会落进其他模块的区间 |
  | `001`～`089` | core：app `001`、log `002`、lock `003` |
  | `090`～`099` | core 的测试（`hygiea-core/tests/` 里自定义的错误 enum） |
  | `100`～`799` | components：db（`RepoErr`）`100`、http-client（`HttpClientErr`）`101` |
  | `800`～`998` | hygiea-test：http_mock `800`（构建）、`801`（运行时） |

- 同一模块内重复会编译报错；跨模块或跨 crate 的重复在启动时检查（进程打印重复的错误码后以状态码 1 退出）。
- 分布式锁的编号（锁类型、字符串 key）用同样的分段和查重规则，但是另一套编号，和错误码互不影响；hygiea 自己的锁编号也按上面内部模块前缀的区间分（core 的测试用 `090`～`099`，db 的测试用 `100`）。
- HTTP 组件（`hygiea::http`）的错误响应：状态码一律 200，错误放在 body 的 `code` / `msg`。业务错误原样返回模板渲染出的消息；`999` 开头的框架内置错误模板参数里可能有内部信息，对外统一换成 `SysErr`（"System Error"），原错误只记在服务端日志里。输入校验这类要给用户看的错误，用项目自己前缀的错误码定义。

<!-- 由 hygiea/tests/err_code_table.rs 生成，不要手改：tools/docs.py gen -->
<!-- gen:err-codes -->
| 错误码 | 变体 | 模板 |
|---|---|---|
| `99900001` | `BaseErr::DateError` | Date error: {{ cause }} |
| `99900002` | `BaseErr::RegexError` | Invalid regex: {{ pattern }} |
| `99900003` | `BaseErr::JsonError` | JSON error: {{ cause }} |
| `99900004` | `BaseErr::TemplateError` | Template error: {{ cause }} |
| `99900005` | `BaseErr::EnvError` | Environment variable not set: {{ name }} |
| `99900006` | `BaseErr::NotPositive` | {{ param }} must be greater than 0: {{ op }} |
| `99900101` | `AppErr::ComponentStartFailed` | component[{{ index }}] {{ type_name }}({{ name }}) failed to start |
| `99900102` | `AppErr::TaskExited` | {{ type_name }}({{ name }}) background task {{ reason }} while running |
| `99900103` | `AppErr::ComponentActivateFailed` | component[{{ index }}] {{ type_name }}({{ name }}) failed to activate |
| `99900111` | `AppErr::ConfigMissing` | Config not set: {{ field }} |
| `99900112` | `AppErr::InvalidConfig` | Invalid config: {{ cause }} |
| `99900113` | `AppErr::ResourceMissing` | Resource missing: {{ resource }} |
| `99900114` | `AppErr::DependencyCycle` | Component dependency cycle: {{ cycle }} |
| `99900115` | `AppErr::DuplicateProvider` | Resource {{ resource }} provided by both {{ first }} and {{ second }} |
| `99900121` | `AppErr::BindFailed` | Bind failed: {{ addr }} |
| `99900122` | `AppErr::ConnectFailed` | Connect failed: {{ target }} |
| `99900123` | `AppErr::InitFailed` | Component init failed: {{ cause }} |
| `99900131` | `AppErr::StopFailed` | Component stop failed: {{ cause }} |
| `99900199` | `AppErr::ComponentError` | Component error: {{ cause }} |
| `99900202` | `BaseLogErr::AppenderFailed` | Create log file appender failed: {{ dir }} |
| `99900203` | `BaseLogErr::InstallFailed` | Install global logger failed |
| `99900204` | `BaseLogErr::FileNameOverlap` | Log layers {{ a }} and {{ b }} have overlapping file names in the same dir |
| `99900205` | `BaseLogErr::InvalidFilter` | Invalid log filter: {{ filter }} |
| `99900206` | `BaseLogErr::NoOutput` | No log output: console is disabled and no file layer is enabled |
| `99900301` | `LockErr::AcquireFailed` | Acquire lock failed: {{ key }} |
| `99900302` | `LockErr::ReleaseFailed` | Release lock failed: {{ key }} |
| `99900303` | `LockErr::InvalidKey` | Invalid lock key: {{ cause }} |
| `99900304` | `LockErr::Unsupported` | Unsupported lock operation: {{ op }} |
| `99900305` | `LockErr::InvalidTimeout` | Invalid lock timeout: {{ cause }} |
| `99910001` | `RepoErr::TransactionBeginFailed` | Db transaction begin failed: {{ op }} |
| `99910002` | `RepoErr::TransactionCommitFailed` | Db transaction commit failed: {{ op }} |
| `99910010` | `RepoErr::ExecFailed` | Db exec failed: {{ op }} |
| `99910011` | `RepoErr::InsertFailed` | Db insert failed: {{ op }} |
| `99910012` | `RepoErr::DeleteFailed` | Db delete failed: {{ op }} |
| `99910013` | `RepoErr::UpdateFailed` | Db update failed: {{ op }} |
| `99910014` | `RepoErr::QueryFailed` | Db query failed: {{ op }} |
| `99910021` | `RepoErr::LiveConfigDefaultsInvalid` | Db live config defaults invalid: {{ ns }} |
| `99910022` | `RepoErr::LiveConfigDecodeFailed` | Db live config decode failed: {{ ns }} |
| `99910023` | `RepoErr::LiveConfigClosed` | Db live config closed |
| `99910024` | `RepoErr::LiveConfigCreateFailed` | Db live config tables create failed: {{ table }} |
| `99910025` | `RepoErr::LiveConfigSchemaMismatch` | Db live config schema mismatch: {{ table }} |
| `99910031` | `RepoErr::LockTableCreateFailed` | Db lock table create failed: {{ table }} |
| `99910032` | `RepoErr::LockTableSchemaMismatch` | Db lock table schema mismatch: {{ table }} |
| `99910091` | `RepoErr::PageOutOfRange` | Db page out of range: page {{ page }}, page_size {{ page_size }} |
| `99910101` | `HttpClientErr::ClientBuildFailed` | Http client build failed |
| `99910111` | `HttpClientErr::InvalidUrl` | Invalid url: {{ url }} |
| `99910112` | `HttpClientErr::InvalidParams` | Invalid query params: {{ method }} {{ url }} |
| `99910113` | `HttpClientErr::InvalidHeader` | Invalid header: {{ cause }} |
| `99910114` | `HttpClientErr::RequestBuildFailed` | Http request build failed: {{ method }} {{ url }} |
| `99910121` | `HttpClientErr::RequestFailed` | Http request failed: {{ method }} {{ url }} |
| `99910122` | `HttpClientErr::NonSuccessStatus` | Http {{ status }}: {{ method }} {{ url }} |
| `99910123` | `HttpClientErr::WriteFailed` | Write response body failed |
| `99910124` | `HttpClientErr::DecodeFailed` | Http response decode failed: {{ method }} {{ url }} |
| `99999999` | `BaseErr::SysErr` | System Error |
<!-- /gen:err-codes -->

## 示例

可运行的示例在 [hygiea-examples](hygiea-examples/README.md)，按主题分目录，每个示例只讲一件事：

```bash
cargo run -p hygiea-examples --example app_basic
```

## 开发

构建、测试的命令和仓库里的约定见 [AGENTS.md](AGENTS.md)。

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
