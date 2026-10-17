# hygiea-examples

hygiea 的使用示例，不发布。按主题分目录（`examples/<主题>/`），每个示例一个文件、只讲一件事，
在 `Cargo.toml` 里用 `[[example]]` 声明，名字是 `<目录>_<文件>`。读配置文件的示例各用 `config/<示例名>/` 一个目录。

```bash
cargo run -p hygiea-examples --example app_basic
```

app 的示例启动后按 Ctrl+C 退出，可以看到组件的关闭顺序：先关 HTTP 这类 Deferred 组件，再按启动的逆序关其余组件。

下面的列表由 `tools/docs.py gen` 生成（每个示例文件 `//!` 的第一段），不要手改：

<!-- gen:examples -->
| 示例 | 讲什么 |
|---|---|
| `app_background_task` | 后台任务和优雅关闭：`startup` 返回任务的 `JoinHandle`，关闭时收到 `CancellationToken` 就退出；不响应的任务等到 `shutdown_timeout` 后被强制结束；`stop` 在任务结束后做收尾。 |
| `app_basic` | 最小的应用：现成的 SQLite 和 HTTP 组件，配置自动从 `hygiea-examples/config/app_basic/dev.toml` 加载。 |
| `app_config` | 配置加载：配置文件、`-f`、环境变量、`--set` 的合并，以及把框架参数合进应用自己的命令行。 |
| `app_dependencies` | 组件依赖：`provides` / `depends_on` 决定启动顺序，跟 `add` 的顺序无关。 |
| `app_global_state` | 全局状态：组件都启动后，在 `before_activate` 里把要用的资源收进一个全局 `AppState`，业务代码直接取，不用一路传 `Resources`。 |
| `app_named_instances` | 同一种组件注册多个实例：主库、从库各一个 SQLite 连接池，按名字区分。 |
| `app_two_phase` | 两阶段启动：`before_activate` 在 HTTP 开始接请求之前执行，适合建表、预热、初始化全局状态。 |
| `datetime_basic` | 日期时间：格式化 / 解析、日 / 周 / 月边界、系统本地时区。 |
| `db_live_config` | 动态配置（数据库配置表）：配置存在库里，改了表，运行中的程序自动拿到新值。 |
| `db_lock` | 分布式锁：PG 连接池组件 `uses` 带 4 时放进 Resources 的 `DbLockClient` 实现 `DistributedLock`。这里不经过 Registry，用 `DbResources::from_config` 直接建两份（`uses = 5`：业务池 + 锁，锁和业务池共用连接），模拟两个实例抢同一把锁。 |
| `env_basic` | 环境变量：内置 key `BuiltinKey`、自定义 `EnvKey`，以及取值的四个函数`env_get` / `env_get_opt` / `env_get_or` / `env_get_or_else`。 |
| `error_basic` | 错误处理：定义错误 enum、err! 的几种写法、错误码。 |
| `http_axum_response` | HTTP 服务的统一响应 / 错误包装（axum 组件）：`AxumHttpResponse` 序列化成 `{code, msg, data}`，`AxumHttpError` 把 `HyErr` 转成响应——业务错误原样把错误码和消息给客户端，框架内置错误（项目前缀`999`）对外一律显示成 "System Error"，原始信息只打在服务端日志里；另外演示了请求体大小限制。 |
| `http_client_basic` | HTTP 客户端：本进程内起一个 axum 服务（随机端口，不连外部服务），用 hygiea 的 http 客户端请求它。控制台日志能看到请求参数里标了 `#[redact(mask)]` 的字段被打码，`#[redact(skip)]` 的字段不出现。 |
| `lock_tokio` | 进程内的锁 `TokioLock`（feature `lock-tokio`）：不连外部服务，同一个进程里的任务互斥，比如单实例部署、本地开发。 |
| `log_basic` | 日志：console + 文件双输出、按 layer 设置不同的 filter、日志时间用 UTC。直接调 `hygiea::log::init`（不经过 `hygiea::app` 组件框架，脚本、小工具也能用）。 |
| `redact_basic` | 日志打码：`#[redact(mask)]` / `#[redact(skip)]`，以及 `to_redacted_json` 和普通 `serde_json`序列化的对比——打码只发生在 `to_redacted_json` 里，存库、回响应、发请求这些普通序列化不受影响。 |
| `string_template` | 模板渲染：fmt_tpl!、fmt_tpl_once!、fmt_pos!。 |
<!-- /gen:examples -->
