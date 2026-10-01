# AGENTS.md

给在这个仓库里写代码的 agent 看的约定。项目介绍和用法见 `README.md`，各 crate 的模块和代码、测试的存放位置见 `docs/architecture.md`，改结构前先读它。

## 项目

基于组件的 Rust 应用框架（`hygiea::app`），附带错误、日志、日期、字符串等基础功能的封装和扩展。使用方只依赖 facade crate `hygiea`。

- `hygiea`：facade，只有 `pub use hygiea_core::*;` 加上按 feature 导出的组件，不放逻辑
- `hygiea-core`：全部核心实现；`hygiea-macros`：过程宏（`hy_err`、`redact`），经由 core 导出
- `hygiea-components/*`：组件（db / redis / http / grpc），依赖 core 的 `app`，只能由 facade 导出
- `hygiea-test`：发布，给使用方写测试用（dev-dependency）；hygiea 自己的测试工具不放这里
- `test-support`（不带 hygiea 前缀，表示仓库内部用）、`hygiea-examples`、`playground`：不发布；`playground` 不在 workspace 里，在它的目录里单独 `cargo test`

项目还在早期，公开 API 可以直接改，不用考虑兼容性。

## 代码规则

- **非测试代码不允许 `unwrap` / `expect`。** 唯一的例外是启动期就会暴露的失败（初始化、加载配置）。其余一律转成 `HyErr` 往上传。
- **依赖库的签名返回 `Result` / `Option` 就当它会失败。** 不要因为读了它当前的实现、觉得这组参数不会失败，就用 `expect` 或把自己的 API 改成不返回 `Result`。只有签名本身不可失败才算不会失败。
- **不替调用方做语义选择。** 底层库返回"唯一 / 歧义 / 不存在"这类结果（比如 `OffsetResult`）时原样返回，不在库里替调用方挑一个。改公开签名、改语义之前先问。
- datetime 的定位：`UtcDateTime` 管 UTC，`OffsetDateTime` 只管系统本地时区，不支持任意时区，不要加 `tz` 参数。
- 错误用 `#[derive(hy_err)]` 定义，错误码规则见 README 的「错误码体系」。框架内置错误用项目前缀 `999`。
- `999` 下只能用 3 位的 `#[err_code_internal_module_prefix = ".."]`（变体码 2 位），宏会拦住 2 位的 `err_code_module_prefix`。
  新加模块按区间取号，取完更新 README「保留和内置错误码」里的表：
  - `000`、`999`：`BaseErr`（不写前缀，5 位码），只用 `000xx` 和兜底的 `99999`
  - `001`～`089`：core（app `001`、log `002`）；`090`～`099`：core 测试里自定义的错误 enum
  - `100`～`799`：components（http-client `100`）
  - `800`～`998`：hygiea-test（http_mock `800` / `801`）
- 测试里的错误 enum 只在自己的测试二进制里注册（linkme 链接期收集），不会进正式程序，
  但会和同一个二进制里链接进来的正式错误一起查重，所以用测试专用区间，别占正式模块的号
- 注释、文档用中文。错误模板等实际输出的字符串保持英文，不翻译。
- 写法跟周围代码保持一致：命名、注释密度、惯用写法。

## feature

- feature 之间的依赖关系只写在 `hygiea-core/Cargo.toml`。facade 的 feature 一对一转发给 core，组件 feature 只引入对应的组件 crate。
- 不常用的功能做成 feature（比如 `redact`），常开的只放错误、日期、环境变量、字符串这类基础功能。
- 命名：reqwest 客户端用 `http-client-reqwest`，实现在 `hygiea-http-client::reqwest_client`；WebSocket 客户端在 `net::ws_client`；`hygiea::http`、`hygiea::grpc` 是服务端组件。

## 测试和示例

| 内容 | 位置 |
|---|---|
| 单元测试：纯逻辑（配置默认值、解析、builder、`provides()` 这类），可以测私有函数 | 源文件里的 `#[cfg(test)] mod tests` |
| crate 的集成测试：要把组件跑起来、起本地服务或容器的；给使用方看的示范写法（只用公开 API） | `<crate>/tests/` |
| 宏的测试：编译期报错（trybuild），fixture 经宏所在的 facade 使用宏 | `hygiea-macros/tests/`、`hygiea-test-macros/tests/` |
| 给使用方看的示例 | `hygiea-examples/examples/<主题>/`，每个在 `Cargo.toml` 里用 `[[example]]` 声明，名字 `<目录>_<文件>` |
| 调查、速查、暂存代码 | `playground/tests/` |

- 测试按依赖分四层，后两层用 `hygiea_test::{container, live}` 标注（内部和使用方用同一套）：

  | 层 | 内容 | 默认 `cargo test` | agent / CI 跑不跑 | 写法 |
  |---|---|---|---|---|
  | 单元 | 纯逻辑：参数、默认值、配置解析 | 跑 | 跑 | `#[test]` |
  | mock | 进程内的本地服务（`test_support::http_server`、`hygiea-test` 的 http_mock；测建连超时用 `hygiea_test::tcp::silent()`） | 跑 | 跑 | `#[test]` / `#[tokio::test]` |
  | container | 用 `hygiea_test::container::ContainerSpec` 在代码里启动容器（需要 Docker） | 不跑 | 跑（本机要有 Docker） | 模块上标 `#[container]` |
  | live | 连使用者自己控制的真实服务（R2、AWS 等），凭证从环境变量读 | 不跑 | **不跑**，只由用户手动跑 | 模块上标 `#[live(env = ["A", ..])]` |

  模块里照常用 `#[test]` / `#[tokio::test]` 标测试，宏给它们加 `#[ignore]`，并把模块内容挪进层名子模块（测试名 `<模块名>::container::<函数名>`）。
  container 层：`cargo test -p <crate> --features .. -- --ignored ::container::`；live 层：`-- --ignored ::live::`。
  有这两层的测试文件，文件头写明运行命令和需要的环境变量。
  组件用 `hygiea-test = { workspace = true }` 作为 dev-dependency 引入。`hygiea-test` 只依赖 core，
  core 自己的测试不能用它（会出现两份 core）。
- 内部 crate 之间的 dev-dependency：被依赖的 crate 反过来（直接或间接）依赖自己时，形成循环，只能写 path、不写版本
  （`cargo publish` 打包时会去掉没有版本号的 dev-dependency），目前是 `hygiea-macros` → `hygiea`、
  `hygiea-test-macros` → `hygiea-test`。没有循环的用 `workspace = true`，`cargo publish --workspace` 会按依赖关系
  （包括 dev-dependency）排发布顺序。
- 组件测的是我们自己写的部分：对第三方框架的封装和增强，比如自己加的配置项和它怎么转成第三方的配置、
  默认值、错误转换、生命周期（启动、按名字放进 Resources、关闭）、日志打码这类附加行为。
  第三方框架自己的功能（连接池怎么复用、协议细节、它自己的配置项各自的效果）由它自己保证，不逐项测；
  只用一两个测试确认接上了、能用就行（比如连上后执行一条查询）。不为了凑覆盖率去测第三方的行为。
- 组件的测试照这个结构写（新加组件直接参考，`hygiea-aws` 是完整的例子）：

  ```
  hygiea-components/hygiea-xxx/
  ├── src/…              #[cfg(test)] mod tests：纯逻辑的单元测试
  └── tests/
      ├── component.rs   每个 crate 都要有，覆盖 crate 里的每个组件，只写基础的：
      │                  按配置构造组件、检查 provides()；不依赖外部服务的再经 Registry 启动一次，
      │                  匿名 / 具名各取一次资源。依赖外部服务的（PG、Redis）这里不启动，
      │                  启动并连上的测试放进 <服务>.rs 的 container 模块
      └── <服务>.rs       连真实服务，比如 pg.rs / redis.rs / s3.rs：
                         #[container] mod <服务名> { .. }      用 ContainerSpec 起容器，镜像写固定 tag
                         #[live(env = [..])] mod <名字> { .. }  托管服务和自建容器行为有差别时才写（比如 R2）
                         两个模块共用同一份测试逻辑，文件头写明运行命令和环境变量
  ```
- 示例不连外部服务，数据库用 SQLite 内存库（`database = ":memory:"`）。
- 除 `hygiea-examples` 外不建 `examples/` 目录。

## 怎么验证

- 改代码过程中只跑改动对应的测试，带上相关 feature，比如 `cargo test -p hygiea-http-client --features reqwest --test reqwest_client`。不要默认跑 `--workspace --all-features` 全量测试，同一条命令不要重复跑。
- 准备提交时，先分析这次改动需要新增或修改哪些测试，确认后再按范围测。
- 改了 feature 或 `cfg`，用几种 feature 组合 `cargo check`（不开、只开相关的、`--all-features`）。
- 改了有 container 层测试的代码，本机有 Docker 时顺带跑对应 crate 的 container 层（`-- --ignored ::container::`）；
  没有 Docker 就说明没跑。live 层不要跑，需要用户的凭证，改了相关代码时告诉用户要手动跑哪条命令。
- 仓库目前没有 CI。以后加 CI 时：普通测试 `cargo test --workspace --all-features`；有 Docker 的 runner 上再跑
  `cargo test --workspace --all-features -- --ignored ::container::`；live 层不放进 CI。
- 改了宏的报错信息，重新生成 trybuild 快照并检查 diff：
  `TRYBUILD=overwrite cargo test -p hygiea-macros --test hy_err_ui --test redact_ui`
  （`#[container]` / `#[live]` 的快照：`TRYBUILD=overwrite cargo test -p hygiea-test-macros --test test_tier_ui`）

## 发版

- 用 `release/release.sh` 发版：升版本号（`cargo set-version`）、改 README 里的版本号、提交、`cargo publish --workspace`、
  打 tag、push。先 `release/release.sh --dry-run --bump alpha` 检查，再去掉 `--dry-run` 执行。
  被 crates.io 限流时脚本自己等待重试；其他原因中断的，修好后用 `release/release.sh --resume` 接着发。用法见脚本开头。
- 平时改代码不要求同步 `docs/architecture.md`，但每次发新版本前必须核对一遍，让它和实际结构一致：crate 列表、各 crate 的模块和文件、feature 与 facade 的转发关系、测试和示例放在哪。以 `Cargo.toml` 和源码为准，文档跟着改。
- 发版前同样核对 README 的 feature 表、组件表、示例表是否和 `Cargo.toml`、`hygiea-examples` 一致。
- 改了框架或组件的配置结构体，同步改 `docs/config-template.toml`；`cargo test -p hygiea --all-features --test config_template` 会检查两者是否一致。

## 沟通

- 如实陈述方案的能力、限制和工作量，不替用户判断需求轻重（不说"过度设计""太重"）。
- 问第三方库的行为就只答第三方库，不顺带分析本项目的代码。
- 演示某个 API 的用法时只用这个 API 本身，不要混入项目里的 wrapper。
