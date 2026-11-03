# AGENTS.md

给在这个仓库里写代码的 agent 看的约定：规则、结构约定和从哪里开始看。项目介绍和用法见 `README.md`，
各模块是什么、怎么用写在它自己的 `//!` 里，`tools/docs.py tree [路径]` 列出目录和每个文件 `//!` 的第一段。

## 项目

基于组件的 Rust 应用框架（`hygiea::app`），附带错误、日志、日期、字符串等基础功能的封装和扩展。使用方只依赖 facade crate `hygiea`。

- `hygiea`：facade，只有 `pub use hygiea_core::*;` 加上按 feature 导出的组件，不放逻辑
- `hygiea-core`：全部核心实现；`hygiea-macros`：过程宏（`hy_err`、锁的两个 derive、`component`、`redact`、`hy_live_config`），经由 core 导出
- `hygiea-components/*`：组件（db / redis / http / http-client / grpc / aws），依赖 core 的 `app`，只能由 facade 导出（core 不能依赖组件）
- `hygiea-test`：发布，给使用方写测试用（dev-dependency），只依赖 core（不经过 facade、不依赖组件），所以组件的测试能用它、core 自己的测试不能用；
  `hygiea-test-macros` 是它的过程宏，只改写代码结构，运行时逻辑在 `hygiea_test::tier`。hygiea 自己的测试工具不放这里
- `test-support`（不带 hygiea 前缀，表示仓库内部用）、`hygiea-examples`、`playground`：不发布
  - `test-support` 不单独建 `tests/`：它在各 crate 的测试里被用到，出问题那些测试就会失败；有独立逻辑时在对应文件里写单元测试
  - `playground` 不在 workspace 里（根 `Cargo.toml` 的 `exclude`），有自己的 `Cargo.lock`（不提交）和 `target/`，在它的目录里单独 `cargo test`。
    只有 `tests/`，`src/lib.rs` 是空的（Cargo 要求至少有一个 lib 或 bin），里面的代码不保证一直能编译

项目还在早期，公开 API 可以直接改，不用考虑兼容性。

## 从哪里开始看

| 主题 | 先读 | 内部机制 |
|---|---|---|
| 应用框架（Registry、组件、Resources、配置加载、关闭） | `hygiea-core/src/app/mod.rs` | `docs/design/app.md` |
| 错误和错误码 | `hygiea-core/src/error.rs`、`hygiea-macros/src/error.rs`；规则见 README「错误码体系」 | |
| 分布式锁 | trait 和 key：`hygiea-core/src/lock/mod.rs`；PG / MySQL：`hygiea-db/src/lock/mod.rs` | `docs/design/lock.md` |
| 动态配置 | `hygiea-db/src/live_config/mod.rs`、`hygiea-macros/src/live_config.rs` | `docs/design/live-config.md` |
| 数据库组件 | `hygiea-db/src/lib.rs`、`component.rs`（`uses`、建和关的顺序）、`build.rs`（feature 组合的 cfg 别名） | |
| HTTP 客户端 | `hygiea-http-client/src/reqwest_client/mod.rs` | `docs/design/http-client.md` |
| S3 | `hygiea-aws/src/s3/mod.rs` | `docs/design/aws-s3.md` |
| 宏生成的代码怎么引用 hygiea | `hygiea-macros/src/krate.rs` | |
| 测试分层、给使用方的测试工具 | `hygiea-test/src/lib.rs` | 容器：`docs/design/test-container.md` |
| 还没做的、没定的 | `docs/todo/` | |

## 结构约定

改结构前先看这里，具体文件用 `tools/docs.py tree` 看。

- 过程宏生成的代码经 facade 引用（`::hygiea::`，`hygiea-test-macros` 的是 `::hygiea_test::`）：按调用方 Cargo.toml 实际依赖的名字解析
  （`krate.rs`），宏上写 `crate = ".."` 时以它为准。hygiea-db 的 `lib.rs` 有 `extern crate self as hygiea_db`，让宏生成的路径在本 crate 里也能解析
- `#[derive(hy_live_config)]` 定义在 hygiea-macros，经 core 的 `#[doc(hidden)]` 模块 `__live_config` 转出，由 hygiea-db 的 `live_config` 导出
  （`hygiea::db::live_config::hy_live_config`），它实现的 trait 是同一模块里的 `LiveConfigFields`
- `hygiea_core::builtin`（常开、`#[doc(hidden)]`）：hygiea 自己的模块、组件用的保留编号，比如锁编号 999999999、hygiea-db 建表时串行化用的锁
- 组件按能力分 crate，用哪个框架或数据库由 crate 自己的 feature 决定（`hygiea-db` 的 `sqlx`、`postgres`……），facade 的 `<能力>-<框架>` 一对一转发
- hygiea-db：
  - 框架、数据库、功能三个维度互相独立，实现按组合编译。组合条件只写在 `build.rs` 的 cfg 别名里，源码里写 `#[cfg(sqlx_lock_pg)]` 这种别名；
    别名分两层：按驱动的（`sqlx_*`，给驱动目录用）和按数据库的（`lock_*`、`live_config_*`，任意驱动有实现就算，给和驱动无关的规则、SQL 用）
  - 功能（锁、动态配置）按三层放：规则在功能模块里（`lock/`、`live_config/`），SQL 全部在 `sql/<数据库>/`（和驱动无关，换驱动直接复用），
    执行在功能模块下的驱动目录（`sqlx/`，一种数据库一个文件，共用的在 `src/sqlx/internal/`）。加别的驱动只需另写一个和 `sqlx/` 平级的执行目录，
    参考 `research/db-drivers` 分支
  - 连接池一种数据库一个文件（`sqlx/pool/`、`seaorm/pool/`），再导出到 crate 根
- hygiea-http-client：`lib.rs` 按 feature 声明各实现的模块，不在 crate 根上再导出（以后加别的实现时名字不撞）；`reqwest_client/mod.rs` 只放结构说明和再导出
- hygiea-aws：每个服务一个组件、一个 `src/<服务>/` 目录，配置里按名字建多个客户端（S3 的客户端下面再嵌套 bucket）。
  客户端默认只用配置里写的，不读 SDK 自己的环境；读 SDK 环境（aws-config）只在 `sdk-env` feature 下。
  各服务共用的只放在 `src/config.rs`：写死的凭证、按 SDK 环境加载 `SdkConfig`
- 框架和每个组件的配置模板在各自 crate 的 `config/` 下。改了配置结构体同步改模板，结构体所在源文件的单元测试 `template_is_code_defaults`
  会按结构体解析模板（`deny_unknown_fields`）并确认和 `Default` 一致（db 的模板例外：`uses` 写全开、各段都列出来，对照全开的配置）

## 代码规则

- datetime 的定位：`UtcDateTime` 管 UTC，`OffsetDateTime` 只管系统本地时区，不支持任意时区，不要加 `tz` 参数。
- 错误码规则见 README 的「错误码体系」。框架内置错误用项目前缀 `999`。
- `999` 下只能用 3 位的 `#[err_code_internal_module_prefix = ".."]`（变体码 2 位），宏会拦住 2 位的 `err_code_module_prefix`。
  新加模块按区间取号，取完更新 README「保留和内置错误码」的区间表（错误码表本身是生成的，见「文档」）：
  - `000`、`999`：`BaseErr`（不写前缀，5 位码），只用 `000xx` 和兜底的 `99999`
  - `001`～`089`：core（app `001`、log `002`、lock `003`）；`090`～`099`：core 测试里自定义的错误 enum
  - `100`～`799`：components（db `100`、http-client `101`、aws `102`）
  - `800`～`998`：hygiea-test（http_mock `800` / `801`）
- 组件的错误：只看配置值本身就能判断的（解析不了、不在允许的取值里、范围不对、几项互相矛盾、写重了），报 `AppErr::InvalidConfig`，
  具体是哪一项、怎么不对写进 cause，不为它另定义组件自己的变体。运行起来和外部交互出的错（连不上、服务端不存在、没权限），
  以及按名字取资源取不到这类，报组件自己的具体错误，带上能排查的参数。调用方传的参数不合规则的（文件名、key 前缀、有效期这类），
  也报组件自己的变体，不报 `InvalidConfig`：它们不是配置
- 一个配置按开关建多个资源的组件参考 hygiea-db：几种连接池共用一个泛型组件（`DbComponent<C>`），
  不经过 Registry 用 `DbResources::from_config` 一次建出全部

## feature

- feature 之间的依赖关系只写在 `hygiea-core/Cargo.toml`。facade 的 feature 一对一转发给 core，组件 feature 只引入对应的组件 crate。
  例外：`db-live-config` 除了引入 hygiea-db 的 `live-config`，还带上 facade 的 `live-config`（→ core → hygiea-macros，`#[derive(hy_live_config)]`）。
- 不常用的功能做成 feature（比如 `redact`），常开的只放错误、日期、环境变量、字符串这类基础功能。
- 命名：reqwest 客户端用 `http-client-reqwest`，实现在 `hygiea-http-client::reqwest_client`；WebSocket 客户端在 `net::ws_client`；`hygiea::http`、`hygiea::grpc` 是服务端组件。

## 测试和示例

| 内容 | 位置 |
|---|---|
| 单元测试：纯逻辑（配置默认值、解析、builder、`provides()` 这类） | 源文件里的 `#[cfg(test)] mod tests` |
| crate 的集成测试：要把组件跑起来、起本地服务或容器的；给使用方看的示范写法 | `<crate>/tests/` |
| 给使用方看的示例 | `hygiea-examples/examples/<主题>/`，每个示例一个文件、只讲一件事，在 `Cargo.toml` 里用 `[[example]]` 声明（Cargo 只自动发现 `examples/*.rs` 和 `examples/*/main.rs`），名字 `<目录>_<文件>`；读配置文件的示例各用 `config/<示例名>/` 一个目录 |
| 调查、速查、暂存代码 | `playground/tests/` |

- 测试分层见 `hygiea-test` 的 crate 文档。HTTP 客户端的测试在进程里起本地服务（`test_support::http_server`），测建连超时用 `hygiea_test::tcp::silent()`，默认就跑。
  有 container、live 层的测试文件，文件头写明运行命令和需要的环境变量。
  组件用 `hygiea-test = { workspace = true }` 作为 dev-dependency 引入。`hygiea-test` 只依赖 core，
  core 自己的测试不能用它（会出现两份 core）。
- 组件的测试照这个结构写（新加组件直接参考，`hygiea-db` 是完整的例子）：

  ```
  hygiea-components/hygiea-xxx/
  ├── src/…              #[cfg(test)] mod tests：纯逻辑的单元测试。每个组件至少有两个：
  │                      配置模板（config/<组件>.toml，include_str! 读进来）解析后等于配置结构体的 Default；
  │                      provides() 按组件名提供资源（匿名的名字是 ""）。这两种不再单独放进 tests/ 的 component.rs、
  │                      config_template.rs（hygiea-db 的 tests/component.rs 是源文件 src/component.rs 的主测试文件，不是这个）
  └── tests/
      ├── <源文件名>.rs   每个组件一个主测试文件，和组件的源文件同名：hygiea-redis 的 fred_pool.rs、hygiea-http 的
      │                  axum_server.rs、hygiea-aws 的 s3.rs；源文件在子目录里的用框架目录加文件名，hygiea-db 的
      │                  sqlx_postgres.rs（src/sqlx/pool/postgres.rs）、seaorm_postgres.rs、sqlx_mysql.rs、sqlx_sqlite.rs ..，
      │                  6 种连接池共用的组件 src/component.rs 对应 component.rs（7 个组件一起起的综合测试）。
      │                  feature 全开，测这个组件的全部能力，包括经 Registry 启动、取到资源能用、停止时关闭。按主题分模块：
      │                  #[container] mod <主题> { .. }        用 ContainerSpec 起容器，镜像写固定 tag
      │                  #[live(env = [..])] mod <名字> { .. }  托管服务和自建容器行为有差别时才写（比如 R2）
      │                  不需要外部服务的（本地起的服务、SQLite 内存库、本地临时文件）写普通模块，默认的 cargo test 就跑
      ├── <源文件名>/     主测试文件太大时可以做成目录（main.rs 当入口，比如 hygiea-http-client 的 reqwest_client/）
      ├── <其他>.rs       某一块测试多、或者比较特殊的，另开文件（比如 hygiea-http 的 axum_layers.rs）；
      │                  和全开互斥的 feature 放在主测试文件里用 cfg 区分，换一组 feature 再跑一次（hygiea-db 的 sqlx_postgres.rs 不开 lock-row）
      └── common/mod.rs  几个组件共用的测试逻辑
  ```
  文件头写明运行命令（带上这个组件全开的 feature）和环境变量。
- 组件的测试放在组件 crate 自己的 `tests/` 里，不放进 examples 或 playground。除 `hygiea-examples` 外不建 `examples/` 目录。

## 怎么验证

- 只跑改动涉及的 crate（`-p <crate>`），收尾也一样；clippy、rustdoc 同样按 crate 跑。`--workspace` 的全量只在用户要求时跑。
- 测试用 nextest，配置在 `.config/nextest.toml`，默认不跑 trybuild（`_ui` 结尾的测试二进制）和 live 层：
  - 改了的 crate：`cargo nextest run -p <crate> <测试文件头写的 feature> --run-ignored all`（普通测试加 container 层；本机有 Docker，不用问）
  - 改了宏：只跑改到的那个 trybuild（见下面重新生成快照的命令，去掉 `TRYBUILD=overwrite` 就是只跑不改）
  - doc test：可运行的只有 core 里的两个，改到它们时跑 `cargo test -p hygiea-core --all-features --doc`
  - live 层：agent 不跑，不要用 `--ignore-default-filter`、`-E 'test(/::live::/)'` 把它选进来
- 全量（用户要求时）：`cargo nextest run --workspace --all-features --run-ignored all`，Summary 里 skipped 的应该只有 live 层。
  以后加 CI 用：普通测试 `cargo nextest run --workspace --all-features`，有 Docker 的 runner 加 `--run-ignored all`，live 层不放进 CI
- 重新生成 trybuild 快照（只跑改到的那个 `--test`）：
  - hygiea-macros：`TRYBUILD=overwrite cargo test -p hygiea-macros --test hy_err_ui --test redact_ui --test component_ui --test lock_ui --test live_ui`
  - `#[container]` / `#[live]`：`TRYBUILD=overwrite cargo test -p hygiea-test-macros --test test_tier_ui`
  - hygiea-db 的 `db!`：`TRYBUILD=overwrite cargo test -p hygiea-db --all-features --test db_macro_ui`

## 文档

一条信息只放一个地方，放在离它描述的东西最近的位置：

| 写什么 | 放哪 |
|---|---|
| 规则、结构约定、从哪里开始看 | 本文件 |
| 模块、文件是什么，用法、行为、配置、限制、注意事项（使用方看得到、要依赖的） | 它自己的 `//!` / `///`。`//!` 的第一段写一两句话的概括（`tools/docs.py tree` 和示例列表取的就是它），`src/` 下每个 `.rs` 都要有 |
| 内部流程、实现方式、为什么这样做、考虑过不用的方案（使用方看不到的） | `docs/design/<主题>.md`，开头链到对应的模块文档，不重复写行为 |
| 还没做的、没定的，以及为它做的调查 | `docs/todo/<主题>.md`。做完的从 todo 删掉：用法和行为写进 rustdoc，原因和机制写进 design，调查记录随条目删掉、结论留在 design |
| 给使用方的入口 | `README.md`：定位、安装、feature 表和组件表、core 的简短用法、错误码规则；组件每项一句话，细节在组件 crate 的文档 |
| 示例列表 | `hygiea-examples/README.md`，生成的 |

- 一句话里既有行为又有原因的拆开：行为写进 rustdoc，原因写进 design。
- 文件的 `//!` 跟着代码一起改（它是代码的一部分）；README、design、todo 的同步等用户说「收尾」再做。
- README 的组件表、feature 表每项一句话：提供什么（入口类型、资源），不链到源文件（模块文档从本文件「从哪里开始看」找）。方法名、参数、key 怎么拼、常量这类细节
  只写 rustdoc 和 design，收尾时 API 改了不往 README 里补；写之前先看同一张表别的行写到多细，跟它们对齐。
- 不维护目录、文件清单这类文档：要看结构跑 `tools/docs.py tree [路径]`。
- `<!-- gen:xxx -->` 和 `<!-- /gen:xxx -->` 之间是生成的，不要手改：README 的内置错误码表（`hygiea/tests/err_code_table.rs`）、
  示例列表。改了错误码、示例、feature 后跑 `tools/docs.py gen`，再跑 `tools/docs.py check`。
- `tools/docs.py check`（只用 Python 标准库，macOS 自带的 python3 就能跑）检查：生成的部分是不是最新的；`src/` 下的 `.rs` 都有 `//!`；
  README `<!-- check:features -->` 段表格第一列的 feature 和 `hygiea/Cargo.toml` 一一对应；AGENTS.md、README、`docs/design`、`docs/todo`、
  示例 README 里反引号写的仓库路径存在、相对链接能打开；各 crate 的 `config/*.toml` 都列在 README 的模板表里；
  AGENTS.md 重新生成 trybuild 快照的命令覆盖了全部 `*_ui` 测试。错误码表平时跑 `cargo nextest run -p hygiea --all-features` 也会检查。
- 写路径时：反引号里的路径指仓库里的文件，可以写 crate 内的相对路径（`sql/postgres/mod.rs`），能对上仓库里的文件就行；
  指使用方项目的（README「快速开始」，这一节不检查）或第三方源码的不加反引号。
- 工具只做结果确定的事（生成、逐项比对），内容是否重复、写在哪合适由 agent 按上面的规则判断。

## 发版

- 用 `release/release.sh` 发版：检查文档（`tools/docs.py check`）、升版本号（`cargo set-version`）、改 README 里的版本号、提交、`cargo publish --workspace`、
  打 tag、push。先 `release/release.sh --dry-run --bump alpha` 检查，再去掉 `--dry-run` 执行。
  被 crates.io 限流时脚本自己等待重试；其他原因中断的，修好后用 `release/release.sh --resume` 接着发。用法见脚本开头。
- 发版前核对 README 和 design 是否和代码一致（README 的 feature 表、组件表；design 里的流程），以 `Cargo.toml` 和源码为准，文档跟着改。

## 沟通

- 讨论方案时，用户回一句简短的话不等于同意，不要按默认理解直接写下去。只有用户明确说「定了，开始做」才写代码；
  做完先给用户看，不顺手改别的。补文档、同步 README / design / todo、整理提交这类收尾，等用户明确说「收尾」再做。
