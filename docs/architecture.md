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

### hygiea-core 的模块

| 模块 | feature | 内容 |
|---|---|---|
| `error` | 常开 | `HyErr`、`err!` / `bail!`、错误码 |
| `redact` | `redact`（`http-client-reqwest` 会带上） | 日志打码时的序列化支持 |
| `datetime` | 常开；`datetime-iana` 开本地时区，`datetime-chrono` 开 chrono 互转 | 基于 `time` 的格式化、解析、日界计算 |
| `env` | 常开 | 环境变量 |
| `string` | 常开 | 缓存的正则、模板渲染，`fmt_tpl!` / `fmt_tpl_once!` / `fmt_pos!` 宏；`xxhash.rs` 是 `xxh3_64` / `xxh32`（字符串固定地映射成整数，跨进程结果一样） |
| `builtin` | 常开（`#[doc(hidden)]`） | hygiea 内置保留的编号，给 hygiea 自己的模块、组件用：锁编号 `LOCK_TYPE` / `LOCK_CODE`（999999999）和组件自己用的锁 `Lock`（hygiea-db 建表时串行化） |
| `log` | `log` | 日志配置和初始化 |
| `app` | `app` | 应用框架：组件（`Component` 加上 `ImmediateComponent` / `DeferredComponent` 两种启动方式）、注册、依赖排序、两阶段启动（`before_activate` / `on_ready`）、优雅退出；`ConfigResource` + `ImmediateResourceComponent<R>`（一个配置建一个资源的通用组件，`resource_component.rs`）；`Resources` 持有 `RegistryConfig`（`registry_config()`，三个阶段视图都能取，建资源时传给 `from_config`）；`#[component]` 宏从这里导出。设计说明见 `docs/design/app.md` |
| `lock` | `lock` | 分布式锁的抽象：`mod.rs` 是 `DistributedLock` trait（后端最少实现 `try_lock`，其余默认一层调一层）和 `lock_key!`，`key.rs` 是 key 类型（`LockKeyI32` / `LockKeyI64` / `LockType` / `LockKey`）和锁编号的注册（启动查重、字符串 key 的模板），`guard.rs` 是 `LockGuard`，`error.rs` 是 `LockErr`，`tokio_lock.rs` 是进程内的锁 `TokioLock`（feature `lock-tokio`）。设计说明见 `docs/design/lock.md` |
| `net` | `ws-client` | WebSocket 客户端 |

### hygiea-macros

| 文件 | 内容 |
|---|---|
| `src/error.rs` | `#[derive(hy_err)]`：校验错误码和模板，生成错误码注册 |
| `src/lock.rs` | `#[derive(hy_lock_type)]` / `#[derive(hy_lock_key)]`：锁类型和字符串 key 的编号、模板，生成注册项（两个 derive 共用一套编号）。`lock` feature（core 的 `lock` 会打开）开了才编译 |
| `src/prefix.rs`、`src/template.rs` | 带编号的 derive（`hy_err` 和锁的两个）共用的部分：enum 上的前缀属性、Cargo.toml 的项目前缀、999 的规则；模板的语法检查和变量 |
| `src/redact.rs` | `#[redact]`：字段打码、跳过。`redact` feature 开了才编译 |
| `src/component.rs` | `#[component]`：把 `impl ImmediateComponent` / `impl DeferredComponent` 块拆成 `impl Component` 和启动 trait 两个 impl，按 trait 填 `Kind`。`component` feature（core 的 `app` 会打开）开了才编译 |
| `src/krate.rs` | 按调用方的依赖名决定生成代码里用 `::hygiea` 还是 `::hygiea_core`，`crate = "path"` 可以覆盖 |
| `tests/` | trybuild 测试：`hy_err_ui`（`#[derive(hy_err)]`、`err!`）、`redact_ui`（`#[redact]`）、`component_ui`（`#[component]`，包括 Deferred 组件第一阶段读不到资源）、`lock_ui`（锁的两个 derive、`lock_key!`、`try_lock*` 只收注册过的 key）。dev-dependency 依赖 facade（只写 path：facade 反过来依赖本 crate），fixture 用 `hygiea::` 路径，和下游写法一样 |

### 配置模板

框架和每个组件的配置模板放在各自 crate 的 `config/` 下（`hygiea-core/config/registry.toml`、`hygiea-db/config/{sqlx-pg,seaorm-pg,sqlx-sqlite,sqlx-mysql,toasty-pg,toasty-mysql}.toml`、
`hygiea-redis/config/redis.toml` ……），各 crate 的 `tests/config_template.rs` 按自己的配置结构体解析它（`deny_unknown_fields`），
并确认模板里生效的值等于 `Default`。改了配置结构体，在同一个 crate 里改模板。

### 组件

组件按能力划分 crate，具体用哪个框架或数据库由 feature 决定。组件都依赖 core 的 `app`。

| crate | 框架 feature | 其他 feature | facade 的 feature | facade 的路径 |
|---|---|---|---|---|
| `hygiea-db` | `sqlx`、`seaorm`、`toasty` | 数据库 `postgres`、`sqlite`、`mysql`；功能 `live-config`、`lock`、`lock-row`（依赖 `lock`）。三个维度互相独立，实现按组合编译，组合条件集中在 `build.rs` 的 cfg 别名（`sqlx_pg`、`sqlx_lock` 等） | 一对一转发：`db-sqlx`、`db-seaorm`、`db-toasty`、`db-postgres`、`db-sqlite`、`db-mysql`、`db-live-config`、`db-lock`、`db-lock-row`，没有组合 feature | `hygiea::db` |
| `hygiea-redis` | `fred` | | `redis-fred` | `hygiea::redis` |
| `hygiea-http` | `axum` | | `http-axum` | `hygiea::http` |
| `hygiea-http-client` | `reqwest` | | `http-client-reqwest` | `hygiea::http_client`（实现在 `reqwest_client` 子模块） |
| `hygiea-grpc` | `tonic` | | `grpc-tonic` | `hygiea::grpc` |
| `hygiea-aws` | 服务：`s3` | | `aws-s3` | `hygiea::aws` |

| 文件 | 内容 |
|---|---|
| `hygiea-db/src/sqlx/`、`seaorm/`、`toasty/` | 各框架专用的部分（toasty 是占位，只有 `pool/`；`sqlx/internal/` 见下）。功能（锁、动态配置）按三层放：规则在功能模块里，SQL 全部在 `sql/<数据库>/`，执行在功能模块的 `sqlx/` 子目录，里面一种数据库一个文件，和 `sql/<数据库>/` 对应（共用的在 `sqlx/internal/`）；别的驱动只需要在各功能下另写一个和 `sqlx/` 平级的执行目录：`pool/` 是连接池组件（一种数据库一个文件，再导出到 crate 根），`transaction.rs` 是事务函数，`datetime.rs` 是 `HyUtcDateTime` 的框架 trait 实现 |
| `hygiea-db/src/seaorm/db.rs` | `db!` 和 `.db()`（`DbResultExt`）：SeaORM 的 `DbErr` 转成 `RepoErr`，`db!` 按语句类型选错误、op 带表名和调用位置 |
| `hygiea-db/src/seaorm/page.rs`、`model.rs` | `find_page`（取一页再查总数）、`active_models`（`&[Model]` 转成 ActiveModel 给 `insert_many`） |
| `hygiea-db/src/datetime.rs` | `HyUtcDateTime`：给 `UtcDateTime` 包一层（经 `OffsetDateTime` 转换，对应 `timestamptz`），开 `sqlx` 或 `seaorm` 就有，框架 trait 的实现在各框架模块里 |
| `hygiea-db/src/page.rs` | `PageRequest` / `Page`：分页的请求和结果，sqlx 和 SeaORM 共用 |
| `hygiea-db/src/error.rs` | `RepoErr`：按操作分的数据库错误，0x 事务、1x 执行语句（10 兜底，11-14 增删改查）、2x 动态配置、3x 锁表、9x 参数 |
| `hygiea-db/build.rs` | feature 组合的 cfg 别名：按驱动的（`sqlx_pg`——sqlx 或 SeaORM + postgres、`sqlx_sqlite`、`sqlx_mysql`、`toasty_pg`，以及 `sqlx_live_config`、`sqlx_lock_pg`、`toasty_lock_pg` 这类各驱动的功能实现，给驱动目录用）和按数据库的（`live_config_pg`、`lock_pg`、`lock_row_pg`、`lock_any` 等，任意驱动有实现就算，给和驱动无关的规则、SQL 用），源码里按别名写 `#[cfg]` |
| `hygiea-db/src/config.rs` | 连接池配置的公共部分：`PgConnectConfig` / `SqliteConnectConfig` / `MySqlConnectConfig`（`[db.connect]`，连哪个库）、`PoolConfig`（`[db.pool]`，sqlx、SeaORM 共用，toasty 占位没用） |
| `hygiea-db/src/live_config/` | 连接池的数据库配置表（feature `live-config`）。和数据库、驱动无关：`mod.rs` 配置和对外导出，`source.rs` 绑定入口，`handle.rs` 句柄（`LiveConfig` / `LiveView`），`worker.rs` 后台任务（和后端之间交换的 `Loaded`、`Signal` 也在这里），`schema.rs` 表名、检查规则、各库语句的集合 `Sql`。`store.rs` 的 `Store` 是 worker 和驱动之间的接口，按驱动分发。执行：`sqlx/`，一种数据库一个文件（`postgres.rs` 小池上的 listener、查询、检查和建表；`sqlite.rs`、`mysql.rs` 只靠轮询），`mod.rs` 的 `SqlxStore` 分发和转错误，sqlx、SeaORM 的连接池共用；`toasty/`（`postgres.rs`，只靠轮询，用连接池自己的 `Db`），`ToastyPgPool` 用。SQL 在 `sql/<数据库>/live_config.*`。设计说明见 `docs/design/live-config.md` |
| `hygiea-db/src/lock/` | PG 连接池的分布式锁（feature `lock`）。和数据库、驱动无关：`mod.rs` 模块文档、guard 显示的 key，`table.rs` 锁表 `{app_name}_lock`（feature `lock-row`）的名字和检查规则。`mod.rs` 里还有 `impl_pg_lock!`（sqlx、toasty 的 PG 锁方法名一样，共用它）。执行：每个驱动一个目录，里面一种数据库一个文件，`mod.rs` 只做声明和导出。`sqlx/`：`postgres.rs` 是 `PgLock`、锁的事务 `LockTx`、锁表的检查和建表，`SqlxPgPool`、`SeaOrmPgPool` 共用；`mysql.rs` 是 `GET_LOCK` 会话级锁，只实现 `try_lock`。`toasty/`：`postgres.rs` 是 `ToastyPgLock`，`ToastyPgPool` 用。SQL 在 `sql/<数据库>/lock.*` |
| `hygiea-db/src/sql/` | 框架自己的全部 SQL，和驱动无关（同一个数据库换驱动时直接复用），按数据库分目录：`postgres/`、`sqlite/`。按数据库分目录：`postgres/`、`sqlite/`、`mysql/`。每个目录的 `mod.rs` 放功能之间共用的（PG：框架事务的 BEGIN——固定 READ COMMITTED、`SET LOCAL` 关掉会打断等待和断开持有的超时，读表的列，建表串行化的 advisory lock，等锁超时的 SQLSTATE；SQLite：读表的列），各功能一个文件，建表语句和它放在一起：`lock.rs` + `lock.sql`（advisory 语句、`timeout` 转 `lock_timeout` 的校验、锁表的 DDL / INSERT / 期望的列），`live_config.rs` + `live_config.sql`（期望的列、函数和触发器名、各条查询、通知 payload 的格式）。`sql/mod.rs` 放和数据库也无关的：表名前缀的校验、表的列类型 `Columns`。建表时串行化的 advisory lock 的两个数是 `hygiea_core::builtin` 的 `(LOCK_TYPE, Lock::*)` |
| `hygiea-db/src/toasty/internal/` | 锁、动态配置的 toasty 实现共用的执行部分，不对外：动态结果行转成 Rust 类型、框架事务的 BEGIN（`BEGIN` 加 `SET_TIMEOUTS` 两条，toasty 一次只能执行一条）、读表的列、从错误里取 SQLSTATE（经 `tokio-postgres`） |
| `hygiea-db/src/sqlx/internal/` | 锁、动态配置的 sqlx 实现共用的执行部分（现在是读表的列），不对外。只开 SeaORM 加锁或动态配置时 `sqlx` 模块也会编译，但没有公开的东西 |
| `hygiea-redis/src/fred_pool.rs` | 连接池组件 |
| `hygiea-http/src/axum_server.rs` | HTTP 服务组件 |
| `hygiea-http-client/src/lib.rs` | 按 feature 声明各实现的模块，不在 crate 根上再导出（以后加别的实现时名字不撞） |
| `hygiea-http-client/src/reqwest_client/mod.rs` | 模块结构说明和再导出，不放实现 |
| `hygiea-http-client/src/reqwest_client/client.rs` | `ReqwestConfig`（client 级配置）和 `ReqwestComponent`；`config.rs` 是配置文件里用的请求头、代理类型 |
| `hygiea-http-client/src/reqwest_client/request.rs` | `RequestConfig`（单次请求，纯数据）和 `HttpResponse`；`headers.rs` 请求头与认证，`body.rs` 请求体，`response.rs` 响应体 |
| `hygiea-http-client/src/reqwest_client/send.rs` | 发送流程（开头有流程图）；`retry.rs` 重试；`logging.rs` 日志；`text.rs` 字节转文本；`error.rs` 错误 |
| `hygiea-grpc/src/tonic_server.rs` | gRPC 服务组件 |
| `hygiea-aws/src/config.rs` | `AwsConfig`：所有服务共用的配置（region、endpoint、profile、写死的凭证），加载成 `SdkConfig`；每个服务一个子配置。各服务组件的配置都是它 |
| `hygiea-aws/src/s3.rs` | `S3Config`：S3 专属项；`AwsS3Client`：在 `SdkConfig` 上叠加 S3 专属项后造出的客户端，实现 `ConfigResource`；`AwsS3Component = ImmediateResourceComponent<AwsS3Client>`（feature `s3`） |

### hygiea-test

给使用方写测试用，发布，作为 dev-dependency 引入。只依赖 `hygiea-core`（不经过 facade，也不依赖任何组件），所以组件的测试也能用它；core 自己的测试不能用。功能按 feature 选：

| feature | 模块 | 内容 |
|---|---|---|
| `log` | `log` | `init_once()`：进程里装一次默认日志，给手动跑的联网测试看日志 |
| `http-mock` | `http_mock` | 本地 mock HTTP 服务：TOML cassette + Rhai 脚本 + 全局 state（从 JSON 初始化），底层 httpmock（再导出） |
| `tcp` | `tcp` | `silent()`：只接受 TCP、从不回话的本地服务，用 `https://` 连时 TLS 握手卡住，测建连超时（HTTP mock 管不到连接阶段） |
| `container` | `container` | `ContainerSpec` / `RunningContainer`：在代码里启动任意容器（镜像、环境变量、端口、就绪条件由调用方传），底层 testcontainers |
| （常开） | 根、`tier` | `#[container]` / `#[live]`：测试分层属性宏（来自 `hygiea-test-macros`），分层说明在 crate 文档；`tier` 是宏生成的代码调用的函数（`require_env`） |

`http_mock` 的文件：

| 文件 | 内容 |
|---|---|
| `mod.rs` | `Cassette`（`load`、`db`、`start`）、`Mocked`（地址、读写 state、`assert_valid`）、`cassette_root!` |
| `spec.rs` | cassette 的 TOML 格式；请求匹配字段和 httpmock 同名，翻译成 `When` |
| `handler.rs` | 一条 interaction 的响应：固定序列或 Rhai 脚本，组装响应；脚本出错等问题记进 problems |
| `script.rs` | Rhai 引擎：`request` 对象和 `read_json` / `read_text` |
| `template.rs` | cassette `path` 里的路径模板 `/a/{id}` |
| `error.rs` | `HttpMockErr`（内部模块前缀 800）、`HttpMockRuntimeErr`（801） |

测试在 `tests/http_mock.rs`，示例资源在 `tests/resources/httpmock/common/`（server 一个小商店服务，cases 单接口用例）。
概念、用法、定位见 `hygiea-test/src/http_mock/mod.rs` 的模块文档（`cargo doc -p hygiea-test --features http-mock`）。

### hygiea-test-macros

`hygiea-test` 的过程宏，发布，只经由 `hygiea-test` 使用。宏只改写代码结构，运行时逻辑在 `hygiea_test::tier` 里，生成的代码去调用它。

| 文件 | 内容 |
|---|---|
| `src/test_tier.rs` | `#[container]` / `#[live]`：标在模块上，给里面的测试加 `#[ignore]`、把内容挪进层名子模块；`live(env = ..)` 生成 `hygiea_test::tier::require_env` 调用 |
| `tests/` | trybuild：`test_tier_ui`，fixture 经 `hygiea_test` 使用宏。`hygiea-test` 是只写 path 的 dev-dependency |

### test-support

hygiea 自己各 crate 的测试工具，不发布。名字不带 `hygiea-` 前缀，表示只在仓库内部用。

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
| `examples/app/` | `app_basic`、`app_dependencies`、`app_named_instances`、`app_global_state`、`app_background_task`、`app_config`、`app_two_phase`；`examples/db/`：`db_live_config`、`db_lock`（要 Docker）；`examples/lock/`：`lock_tokio` |
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
| 调查、速查、暂存代码 | `playground/tests/` |

补充约定：

- 组件的测试放在组件 crate 自己的 `tests/` 里，不放进 examples 或 playground。
- 测试分单元、mock、container、live 四层，container 和 live 在模块上标 `#[container]` / `#[live]`，分层和运行命令见 `AGENTS.md`。
- 除 `hygiea-examples` 以外，其他 crate 不建 `examples/`。
- `playground` 是独立的 crate，不在 workspace 里（根 `Cargo.toml` 的 `exclude`），有自己的 `Cargo.lock` 和 `target/`，要在它的目录里执行 `cargo test`。只有 `tests/`。`src/lib.rs` 是空的，留着只是因为 Cargo 要求每个包至少有一个 lib 或 bin。playground 里的代码不保证一直能编译通过。
- `test-support` 不单独建 `tests/`。它会在各 crate 的测试里被用到，出问题那些测试就会失败。里面有独立逻辑时，在对应文件里写单元测试。
