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

### 8. 没有响应缓存

静态资源（js、css、图片）、字典表、配置这类短时间内不变的数据，每次都真的发请求。要在 client 上配缓存容量和规则，单个请求能决定怎么用缓存。

语义是应用层缓存：按请求算 key、按规则的 TTL 过期，不看响应的 `Cache-Control` / `ETag`（要 HTTP 语义缓存得换 reqwest-middleware + http-cache，另说）。

**`Client` 包一层**：组件往 Resources 里放 `HttpClient { inner: reqwest::Client, cache: Option<ResponseCache> }`，替代现在的 `reqwest::Client`；`IntoSender` 的 `&Client` 换成 `&HttpClient`，`send(&client)` 写法不变，要原生 API 用 `client.reqwest()`。破坏性改动：`require::<Client>()` 要改成 `require::<HttpClient>()`。

**client 级别配置**（`ReqwestConfig.cache`，不配就没有缓存，行为和现在完全一样）：

```toml
[http_client.cache]
max_bytes = 67108864          # 总容量，按 body 长度 + 响应头估算，超了按 LRU 淘汰；单条比它大的不缓存
default_ttl_secs = 60         # 规则没写 ttl_secs、或者请求显式指定模式时用

# 按顺序匹配，第一条命中的生效；写了的条件都要满足，没写的不限制
[[http_client.cache.rules]]
suffixes = [".js", ".css", ".woff2", ".png", ".svg"]   # 路径后缀，不看 query，不区分大小写
ttl_secs = 86400

[[http_client.cache.rules]]
hosts = ["config.internal"]                            # 域名完全匹配
path_prefixes = ["/api/dict/", "/api/config/"]
ttl_secs = 300

[[http_client.cache.rules]]
hosts = ["search.partner.com"]
path_prefixes = ["/api/query"]
methods = ["POST"]                                     # 不写默认只匹配 GET
ttl_secs = 30
```

**请求级别**（`RequestConfig.cache: CacheMode`），读和写分开：

| `CacheMode` | 读缓存 | 写缓存 |
|---|---|---|
| `Auto`（默认） | 命中规则才读 | 命中规则才写，TTL 用规则的 |
| `ReadWrite` | 读 | 写（不看规则，TTL 用 `default_ttl_secs`） |
| `Refresh` | 不读，强制发请求 | 写，覆盖旧的 |
| `ReadOnly` | 读 | 不写 |
| `Off` | 不读 | 不写 |

- client 没配缓存时所有模式都等于 `Off`，每次都真的发请求；显式写了 `ReadWrite` / `Refresh` / `ReadOnly` 的打一条 WARN（代码和配置对不上），请求照常发
- 实现上先把 client 级别和请求级别合成 `Option<{ cache, read, write }>`，`None` 走现在的发送流程，一行不改

**缓存 key 和内容**：

- key：method + 拼好 params 的完整 URL + auth 和请求头的哈希 + 请求 body 的哈希（GET 为 0）。认证头算进 key，不同用户的响应不会互相命中；只存哈希，不让 token 明文常驻内存
- 流式请求 body（`RawStream`、`Multipart` 里的文件流）算不了哈希，不走缓存，打 debug 日志
- 存原样的状态码、响应头、body 字节，只存 2xx；命中时按 `FromBytes` 再解码，同一个 URL 可以解成不同类型。`BodyStream` 走缓存编译不过（和重试一样要求 `FromBytes`）
- 命中打 `http cache hit` 日志（method、url、剩余 TTL），日志里分得清哪些请求没真的发出去

**实现**：

- 用 `moka::future::Cache`（`future` feature，只在 `reqwest` feature 下引入）：`weigher` 按字节算容量，`Expiry` 给每条单独 TTL，`try_get_with` 让并发 miss 同一个 key 时只发一次
- 并发去重只在读 + 写（`Auto` 命中规则、`ReadWrite`）时生效；`Refresh` 要强制发，`ReadOnly` 没命中的结果不共享，各发各的
- 带重试时先查缓存，没命中再进重试循环；重试成功的 2xx 照常写入
- 新增 `cache.rs`（`ResponseCache`、`CacheConfig`、`CacheRule`、`CacheMode`、key 计算）；组件按配置创建；同步 `docs/config-template.toml`

**要定的**：

- `rules` 为空时：什么都不缓存（只有显式模式的请求走缓存），还是缓存所有 GET。倾向前者，开了缓存也不会误缓存业务接口
- 要不要支持按响应 `Content-Type` 匹配（URL 没后缀的静态资源）：只能拿到响应后判断，只影响写不影响读
- 请求头存哈希还是存完整内容（哈希理论上会碰撞，概率极低）
- 通配符（`/static/**/*.js`）先不做，前缀 + 后缀组合不够用时再引入 glob

## 已知的使用限制

不打算改，先记下来，写文档或者有人问时用。

- 泛型函数里调用 `send`，params、body、resp 这几个泛型参数都要加 `Send` 约束：`IntoSender::send_request` 返回的 future 标了 `+ Send`，trait 返回的 `impl Future` 不会自动带出 `Send`，只能写在签名上
- 用闭包写重试策略时，参数类型要写全（比如 `cfg: RequestConfig<(), Json<X>>`），编译器没法从 `Retry` trait 反推
- 每次尝试前都 clone 一份配置，包括最后一次。`Json<T>` 的 clone 是 `T` 的深拷贝，body 很大时有开销；`Raw` / `Multipart` 是 `Bytes`，clone 只是引用计数加一
- `IntoSender::send_request` 外部也能调用（trait 方法的可见性跟着 trait 走），只是标了 `#[doc(hidden)]`；要彻底挡住得给它加一个私有类型的参数
