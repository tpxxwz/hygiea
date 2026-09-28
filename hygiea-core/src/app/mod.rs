//! 应用框架：按组件组织应用，管理组件的启动和关闭。
//!
//! # 配置从哪里来
//!
//! [`Registry::load_config`] 按下面的顺序合并配置，**后面的覆盖前面的同名字段**：
//!
//! | 顺序 | 来源 | 写法 | 典型用途 |
//! |---|---|---|---|
//! | 1 | 环境配置文件 | `config/<env>.toml` | 每个环境一份完整配置 |
//! | 2 | 额外配置文件 | `-f local,secret` → `config/local.toml`、`config/secret.toml`，按顺序 | 本机调试、单独放密钥 |
//! | 3 | 环境变量 | `HYGIEA__REGISTRY__TRACING__ROOT_ENV_FILTER=debug` | 容器里改配置，不用改文件、不用重打镜像 |
//! | 4 | 命令行 | `--set registry.tracing.root_env_filter=debug`（可重复） | 临时改一两项 |
//!
//! 环境名 `<env>` 按这个顺序取：命令行 `--env prod` → 环境变量 `HYGIEA_ENV=prod` → 默认 `dev`。
//!
//! 配置文件目录按这个顺序取：命令行 `-d /etc/myapp`（`--config-dir`）→ 代码里
//! `ConfigArgs::from_cli().default_config_dir("/etc/myapp")` → 默认相对当前目录的 `config`。
//!
//! 配置项的层级就是应用顶层配置类型的字段层级。框架的配置（[`RegistryConfig`]）建议放在 `registry` 字段下，
//! 和组件、业务的配置分开，不会重名：
//!
//! ```ignore
//! #[derive(serde::Deserialize)]
//! struct AppConfig {
//!     registry: RegistryConfig,   // [registry]、[registry.tracing]
//!     db: SqlxPgConfig,           // [db]
//! }
//! impl IntoRegistryConfig for AppConfig {
//!     fn registry_config(&self) -> RegistryConfig { self.registry.clone() }
//! }
//! ```
//!
//! ## 环境变量怎么写
//!
//! 前缀（默认 `HYGIEA`）加两个下划线，层级之间也用两个下划线，不区分大小写：
//!
//! ```text
//! 配置项  registry.tracing.root_env_filter
//! 环境变量 HYGIEA__REGISTRY__TRACING__ROOT_ENV_FILTER=debug
//! ```
//!
//! 用两个下划线是因为字段名本身带单下划线（`root_env_filter`）。值会自动识别成数字、布尔，
//! 字段是字符串的照样能用。`HYGIEA_ENV`（单下划线）是选环境用的，不会被当成配置项。
//! 前缀可以换成应用自己的：`ConfigArgs::from_cli().env_prefix("MYAPP")`，
//! 这时对应的是 `MYAPP__...` 和 `MYAPP_ENV`。
//!
//! ## 命令行由应用自己管
//!
//! 框架不解析命令行，只提供 [`ConfigArgs`]（`--env`、`-f` / `--config`、`-d` / `--config-dir`、`--set`）。没有自己参数的应用直接用
//! [`ConfigArgs::from_cli`]；有自己参数的应用把它合进自己的 CLI，这样自定义参数、`--help`、
//! `--version` 都正常：
//!
//! ```ignore
//! use hygiea::app::{ConfigArgs, Registry, clap};
//!
//! #[derive(clap::Parser)]
//! #[command(version, about)]          // 应用自己的版本号、描述
//! struct Cli {
//!     #[command(flatten)]
//!     config: ConfigArgs,             // 框架的 --env / -f / -d / --set
//!     #[arg(long)]
//!     dry_run: bool,                  // 应用自己的参数
//! }
//!
//! let cli = <Cli as clap::Parser>::parse();
//! let (registry, cfg) = Registry::load_config::<AppConfig>(&cli.config);
//! ```
//!
//! 应用自己的参数不能和上面这几个重名（包括短参数 `-f`、`-d`）。clap 只在 debug 构建里检查重名
//! （启动时 panic），release 构建不检查，所以开发时跑一次 debug 就能发现。
//!
//! 启动时会把本次用到的环境、文件、环境变量名、`--set` 的 key 打到 stdout（不打值，值里可能有密码）；
//! 配置出错时把同样的信息加上错误打到 stderr 再 panic。
//!
//! # 组件和两阶段启动
//!
//! 组件分两类，按"什么时候开始工作"区分：
//!
//! - **Immediate**（立即启动）：实现 [`ImmediateComponent`]，第一阶段就启动完成，比如数据库、Redis 连接池
//! - **Deferred**（延迟启动）：实现 [`DeferredComponent`]，第一阶段 `prepare` 只做准备（绑端口、检查配置），
//!   第二阶段 `activate` 才开始工作，比如 HTTP / gRPC 服务、MQ 消费者、定时任务
//!
//! [`Registry::run`] 的顺序是：第一阶段（所有组件的 `startup` / `prepare`）→
//! [`Registry::before_activate`] 的回调 → 第二阶段（Deferred 组件的 `activate`）→ [`Registry::on_ready`] 的回调 → 等退出信号。
//!
//! - `before_activate`：资源都齐了，还没有组件在对外服务，适合建表、预热缓存、把资源收进全局状态
//! - `on_ready`：服务已经在接请求，适合注册到注册中心、打"已就绪"的点
//!
//! Deferred 组件第一阶段拿到的是 [`ResourceSink`]（只能放），第二阶段拿到的是 [`ReadyResources`]（只能读），
//! 在第一阶段读资源编译不过，不用靠约定。
//!
//! 组件的公共部分（`Config`、`build`、`provides` 等）在 [`Component`] 里，用 [`component`] 宏和启动方法写在一起：
//!
//! ```ignore
//! #[component]
//! impl ImmediateComponent for PgComponent {
//!     type Config = PgConfig;
//!     fn build(name: Name, config: PgConfig) -> Self { .. }
//!     fn provides(&self) -> Vec<ResourceId> { vec![ResourceId::named::<PgPool>(self.name.clone())] }
//!     async fn startup(&mut self, state: &Resources, shutdown: CancellationToken) -> .. { .. }
//! }
//!
//! #[component]
//! impl DeferredComponent for HttpComponent {
//!     type Config = HttpConfig;
//!     fn build(name: Name, config: HttpConfig) -> Self { .. }
//!     fn depends_on(&self) -> Vec<ResourceId> { vec![ResourceId::named::<PgPool>("primary")] }
//!     async fn prepare(&mut self, sink: &ResourceSink) -> .. { /* 绑端口 */ }
//!     async fn activate(&mut self, res: ReadyResources, shutdown: CancellationToken) -> .. { /* 开始服务 */ }
//! }
//! ```
//!
//! # 组件依赖
//!
//! 组件之间通过 [`Resources`] 共享资源：先启动的组件 `insert_named` 放进去，后启动的组件
//! [`Resources::require_named`] 取出来。谁先谁后不用靠 `add` 的顺序，组件声明自己提供什么、依赖什么，
//! Registry 排好第一阶段的启动顺序（拓扑排序）：
//!
//! ```ignore
//! // redis 依赖主库
//! #[component]
//! impl ImmediateComponent for RedisComponent {
//!     fn provides(&self) -> Vec<ResourceId> { vec![ResourceId::named::<RedisClient>(self.name.clone())] }
//!     fn depends_on(&self) -> Vec<ResourceId> { vec![ResourceId::named::<DbPool>("primary")] }
//!     // startup 里：let db = state.require_named::<DbPool>("primary")?;
//! }
//! ```
//!
//! - 一个组件可以依赖多个资源，依赖可以一层层串下去；没有依赖关系的组件按 `add` 的顺序
//! - Deferred 组件的依赖只在第二阶段读，那时第一阶段都完成了，所以只检查有没有组件提供，不参与排序
//! - 下面这些都在**启动任何组件之前**报错：依赖的资源没有组件提供（`ResourceMissing`）、
//!   依赖成环（`DependencyCycle`，报出环上的组件）、同一个资源两个组件都提供（`DuplicateProvider`）
//! - 组件声明了 `provides` 却没在第一阶段放进去，算这个组件启动失败
//! - `before_activate` 的回调里放进去的资源不属于任何组件，不能写进 `depends_on`，Deferred 组件在 `activate` 里直接取
//! - hygiea-components 里的连接池组件都按组件名声明了 `provides`，比如
//!   `ResourceId::named::<SqlxPgPool>("primary")`
//!
//! # 优雅关闭
//!
//! 收到 Ctrl+C / SIGTERM（Windows 上还有 Ctrl+Break、关窗口、关机）后，[`Registry::run`]：
//!
//! 1. 先照常服务 `shutdown_delay_secs`（默认 0），让负载均衡器把流量切走；
//! 2. 逐个关组件：先按启动的逆序关 Deferred 组件（HTTP / gRPC 不接新请求，处理完手上的），
//!    再按启动的逆序关 Immediate 组件（数据库最后关，提交完数据）；
//! 3. 每个组件单独计时（默认 15 秒，组件可以自己指定），超时的强制结束，接着关下一个；
//! 4. 关闭中再收到一次信号，直接退出（本地开发时再按一次 Ctrl+C）。
//!
//! 部署到 k8s 时的配置（启动日志里的 `Max shutdown time` 就是第 3 行要超过的值）：
//!
//! ```text
//! registry.shutdown_delay_secs = 5            # 应用配置：等 endpoints 摘除生效
//! terminationGracePeriodSeconds: 60           # k8s：要大于 Max shutdown time，否则关到一半被 SIGKILL
//! ```

use std::borrow::Cow;

mod component;
mod config;
mod error;
mod registry;
mod resources;
mod signal;

pub use component::{Component, Deferred, DeferredComponent, Immediate, ImmediateComponent};
pub use config::{ConfigArgs, IntoRegistryConfig, RegistryConfig};
pub use error::BaseAppErr;
/// 标在 `impl ImmediateComponent for ..` / `impl DeferredComponent for ..` 上，把 [`Component`] 的项
/// （`Config`、`build`、`provides`、`depends_on`、`stop`、`shutdown_timeout`）和启动方法写在一个 impl 块里，
/// 展开成 `impl Component`（按 trait 填好 `Kind`）和启动 trait 的 impl 两个，都带上 `#[async_trait]`。
///
/// 别的 trait 上用、手写 `type Kind` 都会报错。生成代码里 hygiea 的路径按调用方的依赖名找，
/// 找不对时用 `#[component(crate = "::path")]` 指定
pub use hygiea_macros::component;
pub use registry::Registry;
pub use resources::{ReadyResources, Resource, ResourceId, ResourceSink, Resources};

// ---- 再导出 -----------------------------------------------------------------
pub use async_trait::async_trait;
/// 应用定义自己的命令行时用，不用自己再加 clap 依赖
pub use clap;
/// 组件的退出信号，见 [`ImmediateComponent::startup`]。再导出是为了组件不用自己加 tokio-util 依赖
pub use tokio_util::sync::CancellationToken;

/// 组件和资源的名字。字面量（`"primary"`）和运行时得到的 `String`（比如从配置读的）都能直接传，
/// 匿名用 `""`
pub type Name = Cow<'static, str>;
