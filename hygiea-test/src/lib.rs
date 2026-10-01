//! 给使用 hygiea 的项目写测试用的工具，作为 dev-dependency 引入，按 feature 选：
//!
//! | feature | 内容 |
//! |---|---|
//! | `log` | [`log::init_once`]：在测试里装一次 hygiea 的默认日志 |
//! | `http-mock` | [`http_mock`]：本地 mock HTTP 服务，TOML cassette + Rhai 脚本 + 接口约定校验 |
//! | `container` | [`container`](mod@container)：在代码里启动容器，给 container 层的测试用 |
//! | `tcp` | [`tcp`]：socket 层的工具，比如建连卡住的本地地址（测 `connect_timeout`） |
//!
//! 不需要 feature 的：[`container`](macro@container) / [`live`](macro@live) 测试分层属性宏，见下文。
//!
//! hygiea 自己各 crate 的测试工具在 workspace 里的 `test-support`，不发布，这里不包含它
//!
//! # 测试分层
//!
//! | 层 | 内容 | 默认 `cargo test` | 写法 |
//! |---|---|---|---|
//! | 单元 | 纯逻辑：参数、默认值、配置解析 | 跑 | `#[test]` |
//! | mock | 进程内的本地服务，比如 `http_mock` | 跑 | `#[test]` / `#[tokio::test]` |
//! | container | 在代码里启动容器（Docker 等），CI 能跑 | 不跑 | [`#[container]`](macro@container) |
//! | live | 连使用者自己控制的真实服务，凭证从环境变量读 | 不跑 | [`#[live]`](macro@live) |
//!
//! 后两层的宏标在模块上。模块里照常用 `#[test]` / `#[tokio::test]` 标测试，宏给它们加上 `#[ignore]`，
//! 没有测试属性的函数（辅助函数）原样保留：
//!
//! ```ignore
//! #[hygiea_test::container]
//! mod rustfs {
//!     use super::*;
//!
//!     #[tokio::test]
//!     async fn put_get() { /* 测试名：rustfs::container::put_get */ }
//! }
//!
//! #[hygiea_test::live(env = ["S3_BUCKET", "S3_ACCESS_KEY_ID"])]
//! mod own_bucket {
//!     use super::*;
//!
//!     #[tokio::test]
//!     async fn put_get() { /* 运行前检查环境变量，缺了列出全部缺少的名字 */ }
//! }
//! ```
//!
//! ```sh
//! cargo test -- --ignored ::container::   # CI 里跑 container 层
//! cargo test -- --ignored ::live::        # 手动跑 live 层
//! ```
//!
//! 为了让层名出现在测试路径里，宏把模块内容挪进一层以层名命名的子模块。外面那层 glob 引入了上一级，
//! 所以模块里的 `super::xxx`、`use super::*` 照常能用；只有往上跳两级以上的 `super::super::..`
//! 要多写一个 `super::`。只能标在内联模块（`mod x { .. }`）上。

#[cfg(feature = "log")]
pub mod log;

#[cfg(feature = "http-mock")]
pub mod http_mock;

// 测试分层宏生成的代码写 `::hygiea_test::..`，在本 crate 自己的测试里也要能解析
extern crate self as hygiea_test;

pub mod tier;

#[cfg(feature = "container")]
pub mod container;

#[cfg(feature = "tcp")]
pub mod tcp;

pub use hygiea_test_macros::{container, live};
