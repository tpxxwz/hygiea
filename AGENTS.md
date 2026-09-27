# AGENTS.md

给在这个仓库里写代码的 agent 看的约定。项目介绍和用法见 `README.md`，各 crate 的模块和代码、测试的存放位置见 `docs/architecture.md`，改结构前先读它。

## 项目

基于组件的 Rust 应用框架（`hygiea::app`），附带错误、日志、日期、字符串等基础功能的封装和扩展。使用方只依赖 facade crate `hygiea`。

- `hygiea`：facade，只有 `pub use hygiea_core::*;` 加上按 feature 导出的组件，不放逻辑
- `hygiea-core`：全部核心实现；`hygiea-macros`：过程宏（`hy_err`、`redact`），经由 core 导出
- `hygiea-components/*`：组件（db / redis / http / grpc），依赖 core 的 `app`，只能由 facade 导出
- `hygiea-test-support`、`hygiea-examples`、`hygiea-playground`：不发布；`hygiea-playground` 不在 workspace 里，在它的目录里单独 `cargo test`

项目还在早期，公开 API 可以直接改，不用考虑兼容性。

## 代码规则

- **非测试代码不允许 `unwrap` / `expect`。** 唯一的例外是启动期就会暴露的失败（初始化、加载配置）。其余一律转成 `HyErr` 往上传。
- **依赖库的签名返回 `Result` / `Option` 就当它会失败。** 不要因为读了它当前的实现、觉得这组参数不会失败，就用 `expect` 或把自己的 API 改成不返回 `Result`。只有签名本身不可失败才算不会失败。
- **不替调用方做语义选择。** 底层库返回"唯一 / 歧义 / 不存在"这类结果（比如 `OffsetResult`）时原样返回，不在库里替调用方挑一个。改公开签名、改语义之前先问。
- datetime 的定位：`UtcDateTime` 管 UTC，`OffsetDateTime` 只管系统本地时区，不支持任意时区，不要加 `tz` 参数。
- 错误用 `#[derive(hy_err)]` 定义，错误码规则见 README 的「错误码体系」。框架内置错误用项目前缀 `999`。
- 注释、文档用中文。错误模板等实际输出的字符串保持英文，不翻译。
- 写法跟周围代码保持一致：命名、注释密度、惯用写法。

## feature

- feature 之间的依赖关系只写在 `hygiea-core/Cargo.toml`。facade 的 feature 一对一转发给 core，组件 feature 只引入对应的组件 crate。
- 不常用的功能做成 feature（比如 `redact`），常开的只放错误、日期、环境变量、字符串这类基础功能。
- 命名：reqwest 客户端用 `http-client-reqwest`，实现在 `hygiea-http-client::reqwest_client`；WebSocket 客户端在 `net::ws_client`；`hygiea::http`、`hygiea::grpc` 是服务端组件。

## 测试和示例

| 内容 | 位置 |
|---|---|
| 单元测试 | 源文件里的 `#[cfg(test)] mod tests` |
| crate 的集成测试（组件的也放这里） | `<crate>/tests/` |
| 宏的测试：编译期报错（trybuild），fixture 经 facade 使用宏 | `hygiea-macros/tests/` |
| 给使用方看的示例 | `hygiea-examples/examples/<主题>/`，每个在 `Cargo.toml` 里用 `[[example]]` 声明，名字 `<目录>_<文件>` |
| 调查、速查、暂存代码 | `hygiea-playground/tests/` |

- 需要真实服务（PostgreSQL、Redis）或联网的测试标 `#[ignore = "..."]`，文件头写明手动运行的命令。
- 示例不连外部服务，数据库用 SQLite 内存库（`database = ":memory:"`）。
- 除 `hygiea-examples` 外不建 `examples/` 目录。

## 怎么验证

- 改代码过程中只跑改动对应的测试，带上相关 feature，比如 `cargo test -p hygiea-http-client --features reqwest --test reqwest_client`。不要默认跑 `--workspace --all-features` 全量测试，同一条命令不要重复跑。
- 准备提交时，先分析这次改动需要新增或修改哪些测试，确认后再按范围测。
- 改了 feature 或 `cfg`，用几种 feature 组合 `cargo check`（不开、只开相关的、`--all-features`）。
- 改了宏的报错信息，重新生成 trybuild 快照并检查 diff：
  `TRYBUILD=overwrite cargo test -p hygiea-macros --test hy_err_ui --test redact_ui`

## 发版

- 平时改代码不要求同步 `docs/architecture.md`，但每次发新版本前必须核对一遍，让它和实际结构一致：crate 列表、各 crate 的模块和文件、feature 与 facade 的转发关系、测试和示例放在哪。以 `Cargo.toml` 和源码为准，文档跟着改。
- 发版前同样核对 README 的 feature 表、组件表、示例表是否和 `Cargo.toml`、`hygiea-examples` 一致。
- 改了框架或组件的配置结构体，同步改 `docs/config-template.toml`；`cargo test -p hygiea --all-features --test config_template` 会检查两者是否一致。

## 沟通

- 如实陈述方案的能力、限制和工作量，不替用户判断需求轻重（不说"过度设计""太重"）。
- 问第三方库的行为就只答第三方库，不顺带分析本项目的代码。
- 演示某个 API 的用法时只用这个 API 本身，不要混入项目里的 wrapper。
