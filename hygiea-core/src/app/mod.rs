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
//! # 组件依赖
//!
//! 组件之间通过 [`Resources`] 共享资源：先启动的组件 `insert_named` 放进去，后启动的组件
//! [`Resources::require_named`] 取出来。谁先谁后不用靠 `add` 的顺序，组件声明自己提供什么、依赖什么，
//! Registry 排好启动顺序（拓扑排序），关闭时按启动的逆序：
//!
//! ```ignore
//! // redis 依赖主库；http 依赖主库和 redis
//! impl Component for RedisComponent {
//!     fn provides(&self) -> Vec<ResourceId> { vec![ResourceId::named::<RedisClient>(self.name.clone())] }
//!     fn depends_on(&self) -> Vec<ResourceId> { vec![ResourceId::named::<DbPool>("primary")] }
//!     // startup 里：let db = state.require_named::<DbPool>("primary")?;
//! }
//! ```
//!
//! - 一个组件可以依赖多个资源，依赖可以一层层串下去；没有依赖关系的组件按 `add` 的顺序
//! - 下面这些都在**启动任何组件之前**报错：依赖的资源没有组件提供（`ResourceMissing`）、
//!   依赖成环（`DependencyCycle`，报出环上的组件）、同一个资源两个组件都提供（`DuplicateProvider`）
//! - 组件声明了 `provides` 却没在 `startup` 里放进去，算这个组件启动失败
//! - hygiea-components 里的连接池组件都按组件名声明了 `provides`，比如
//!   `ResourceId::named::<SqlxPgPool>("primary")`
//!
//! # 优雅关闭
//!
//! 收到 Ctrl+C / SIGTERM（Windows 上还有 Ctrl+Break、关窗口、关机）后，[`Registry::run`]：
//!
//! 1. 先照常服务 `shutdown_delay_secs`（默认 0），让负载均衡器把流量切走；
//! 2. 按启动的**逆序**逐个关组件：先停 HTTP / gRPC（不接新请求，处理完手上的），后停数据库（提交完数据）；
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

pub use component::Component;
pub use config::{ConfigArgs, IntoRegistryConfig, RegistryConfig};
pub use error::BaseAppErr;
pub use registry::Registry;
pub use resources::{Resource, ResourceId, Resources};

// ---- 再导出 -----------------------------------------------------------------
pub use async_trait::async_trait;
/// 应用定义自己的命令行时用，不用自己再加 clap 依赖
pub use clap;
/// 组件的退出信号，见 [`Component::startup`]。再导出是为了组件不用自己加 tokio-util 依赖
pub use tokio_util::sync::CancellationToken;

/// 组件和资源的名字。字面量（`"primary"`）和运行时得到的 `String`（比如从配置读的）都能直接传，
/// 匿名用 `""`
pub type Name = Cow<'static, str>;
