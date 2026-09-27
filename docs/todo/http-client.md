# hygiea-http-client 待办

`hygiea-http-client`（`hygiea::http_client::reqwest_client`）目前缺的功能和已知问题。按优先级排序，前三条排查线上问题时最先碰到。

## 缺的功能

### 1. 错误和日志里的 body 不限长度

非 2xx 和解码失败时，body 原文整个放进 err_args 的 `body`，失败日志的 `resp` 也整个打出来。对方回一个几 MB 的错误页，错误对象和日志都会跟着变大。

- 位置：`send.rs` 的 `SendCtx::status_failed` / `decode_failed`（`text.rs` 的 `body_text`、`body_preview`）
- 要定的：截断长度（固定值还是 `ReqwestConfig` / `RequestConfig` 上可配）；截断后怎么标（比如末尾加 `...<N bytes truncated>`）；`FailStage::Status` / `Decode` 里的原始字节不截断

### 2. 重试时日志看不出是第几次

每次尝试各打一组 `http call start` / `failed`，没有 `attempt` 字段，也没有把同一次调用的多次尝试串起来的标识。线上看到 3 条失败日志，分不清是 3 个请求还是 1 个请求重试了 3 次。

- 位置：`retry.rs` 的重试循环；`logging.rs` 的 `Start` / `Success` / `Failure` 字段
- 做法：日志字段加 `attempt`（不重试时不输出，或者恒为 1），可以和第 6 条的 span 一起做

### 3. 没有跨重试的总时限

`timeout` 只管单次尝试。重试 3 次加上退避，总耗时可能远超预期，现在只能由调用方在外面套 `tokio::time::timeout`，而外面套的超时一到会直接丢弃 future，拿不到最后一次的错误。

- 位置：`RetryCtx`
- 做法：`RetryCtx` 加可选的总时限（比如 `deadline` / `max_elapsed`）；到期后不再重试，返回最后一次的错误；本次尝试的 timeout 是否按剩余时间收紧要定

### 4. 没有现成的退避工具

指数退避、抖动、读 `Retry-After` 头都要调用方在策略里自己写，写法基本一样。

- 做法：提供几个函数（不是策略，不替调用方做判断），比如 `backoff(attempt, base, max)`、`with_jitter(duration)`、`retry_after(&HttpResponse<Bytes>) -> Option<Duration>`（支持秒数和 HTTP 日期两种格式）

### 5. 没有「每个请求都要做」的扩展点

统一签名、统一加 trace header 这类需求，现在只能每个请求手动加，或者走 reqwest 底层的 `connector_layer`（拿不到本模块的 `RequestConfig`）。有签名类接口时会很快遇到。

- 要定的：挂在 `Client` 上还是 `RequestConfig` 上；能不能读 / 改最终的 `reqwest::Request`（签名要看最终的 URL、body）；和重试的关系（每次重试都要重新签名，时间戳会变）

### 6. 没有 tracing span

日志是一条条独立的 event，没有给每个请求开 span，和上游的调用链关联不起来。

- 做法：`send` 里开一个 span（带 method、打码后的 url），start / success / failed 都在 span 内；重试时整个循环一个 span，每次尝试一个子 span 或者带 `attempt` 字段

### 7. 编译期约束没有测试

下面这些保证靠类型约束实现，但没有 trybuild 用例，重构时可能悄悄失效：

- 流式 body（`RawStream`、`multipart::Form`）不能开重试
- `BodyStream` 不能用于重试
- 外部 crate 不能实现 `IntoSender`

- 做法：在 `hygiea-http-client/tests/` 加 trybuild 用例（参考 `hygiea-macros/tests/` 的写法）

## 已知的使用限制

不打算改，先记下来，写文档或者有人问时用。

- 泛型函数里调用 `send`，params、body、resp 这几个泛型参数都要加 `Send` 约束：`IntoSender::send_request` 返回的 future 标了 `+ Send`，trait 返回的 `impl Future` 不会自动带出 `Send`，只能写在签名上
- 用闭包写重试策略时，参数类型要写全（比如 `cfg: RequestConfig<(), Json<X>>`），编译器没法从 `Retry` trait 反推
- 每次尝试前都 clone 一份配置，包括最后一次。`Json<T>` 的 clone 是 `T` 的深拷贝，body 很大时有开销；`Raw` / `Multipart` 是 `Bytes`，clone 只是引用计数加一
- `IntoSender::send_request` 外部也能调用（trait 方法的可见性跟着 trait 走），只是标了 `#[doc(hidden)]`；要彻底挡住得给它加一个私有类型的参数
