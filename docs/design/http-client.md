# HTTP 客户端（reqwest）：内部流程

怎么用（请求、解码器、打码、重试、debug 日志）、限制都在模块文档里：
`hygiea-components/hygiea-http-client/src/reqwest_client/mod.rs`（docs.rs 上是 `hygiea::http_client::reqwest_client`），
各类型、方法的细节在它们自己的文档。这里只讲内部怎么做、为什么这样做。还没做的在 [docs/todo/http-client.md](../todo/http-client.md)。

---

## 1. 模块之间的依赖

`retry → send → request / headers / body / response / logging / text / error`，`client → config`。
`send` 和 `response` 互相引用：`FromBody::from_body` 的参数 `RespBody` 绑定着正在进行的那次发送，读 body 的 `RespBody` 放在 `send.rs`。

## 2. 发送流程

对外只有一个 `send`，第二个参数的类型决定走哪条路。几个函数各管一段：

| 函数 | 位置 | 管什么 |
|---|---|---|
| `send(sender)` | send.rs，pub | 唯一入口，原样转给 `sender.send_request(self)` |
| `IntoSender::send_request` | retry.rs | 按 `sender` 的类型分派：`&ReqwestClient` 发一次，`(&ReqwestClient, RetryCtx)` 带重试 |
| `send_once` | send.rs | 发一次，响应按 `FromBody` 取（可以是流），失败只要 `HyErr` |
| `send_once_bytes` | send.rs | 发一次，响应读完按 `FromBytes` 解码，失败带 `SendFailure`（原始响应），给重试用 |
| `dispatch` | send.rs | 两条路共用的前半段：发出之前逐项检查、构建、打 start 日志、发出去拿到响应头 |
| `read_status_failure` | send.rs | 非 2xx：读完 body，交给 `SendCtx::status_failed` |
| `SendCtx::*_failed` / `succeeded` | send.rs | 调 logging.rs 打失败 / 成功日志，构造错误和 `SendFailure` |
| `RespBody::read_all` / `decode` | send.rs | 读 body、按 `FromBytes` 解码，失败交给 `SendCtx` |

```text
cfg.send(sender)
 └─ sender.send_request(cfg)                            IntoSender，retry.rs：按 sender 的类型二选一
     │
     ├─ &ReqwestClient ────────────────────────────────────── 不重试
     │   └─ send_once::<Decoder: FromBody>
     │       ├─ dispatch ───────────────────────────────┐ 共用的前半段（见下）
     │       ├─ 非 2xx → read_status_failure → Err(NonSuccessStatus)
     │       ├─ 2xx    → Decoder::from_body(RespBody)
     │       │           ├─ FromBytes 类型：RespBody::decode → 失败 Err(DecodeFailed)
     │       │           └─ BodyStream：into_stream，不读 body
     │       └─ 成功   → SendCtx::succeeded（打 success 日志）
     │
     └─ (&ReqwestClient, RetryCtx) ─────────────────────────── 带重试，要求 Params / Req: Clone、Decoder: FromBytes
         └─ loop {
              kept = cfg.clone()                        发出去的那份会被消费，先留一份
              send_once_bytes::<Decoder: FromBytes>
               ├─ dispatch ─────────────────────────────┤ 外层 Err（发出之前的错误）→ 直接返回，不进 Retry
               ├─ 非 2xx → read_status_failure → SendFailure { Status(原始响应) }
               ├─ 2xx    → RespBody::decode → 失败 SendFailure { Decode(原始响应) }
               └─ 成功   → SendCtx::succeeded → 直接返回 Ok
              次数用完 → 返回 failure.err
              retry.retry(attempt, kept, failure)
               ├─ Retry(cfg) / Unknown(cfg) → cfg = 它，继续循环
               └─ Stop(err) → 返回 err
            }

dispatch(cfg, client)                                   两条路共用
 ├─ 发出之前（外层 Err，不打日志）：URL → 日志预览（params / body 打码）→ params 编码 → into_request（认证头、body）→ build
 ├─ 打 http call start
 └─ client.execute
     ├─ 失败 → 内层 Err：SendCtx::transport_failed → SendFailure { Transport }
     └─ 成功 → (SendCtx, 响应头已到的 Response)
```

为什么拆成两条路：不重试时响应可以是流（`BodyStream`），读不读 body 由 `Decoder` 决定；
重试时失败要把原始响应交给 `Retry`，body 必须先整个读进内存，所以只接受 `FromBytes`，失败带 `SendFailure`。
两条路的前半段（`dispatch`）、非 2xx 处理、日志都是同一套，行为一致。

## 3. 日志里的文本和打码

| 函数（`text.rs`） | 作用 | 实现 |
|---|---|---|
| `is_text_content_type(ct)` | 这个 content-type 算不算文本 | 按 MIME 类型判断 |
| `decode_charset(ct, bytes)` | 按 ct 里的 `charset` 把字节解码成文本，默认 UTF-8 | 解码用 `encoding_rs` |
| `body_text(headers, body)` | 响应 body 转文本：文本类解码，二进制只给 `<N bytes ..>`；**不转义** | |
| `to_one_line(text)` | 让日志保持一行：`\n` `\r` `\t` 换成空格，其他控制字符转义 | 单字符转义用 `char::escape_default` |
| `body_preview(headers, body)` | 响应的日志摘要，就是 `to_one_line(body_text(..))` | |

标准库的 `str::escape_default` 会转义所有非 ASCII（中文变成 `\u{..}`），`str::escape_debug` 会转义
引号和反斜杠（JSON 里全是 `\"`），都不适合日志，所以 `to_one_line` 自己实现。
打码用的 `redact::to_redacted_json` 在 `hygiea_core::redact`。

### 调用链

```text
请求（日志 req=）
  send ─ req_preview
    ├─ params → redact::to_redacted_json（打码）
    └─ body   → IntoBody::preview()                      (body.rs)
          ├─ Json / Form → redact::to_redacted_json（打码）
          └─ Raw → is_text_content_type(ContentType::as_str())
                ├─ 文本   → to_one_line(decode_charset(..))
                └─ 二进制 → "<N bytes type>"

响应（send 拿到 body 之后）
  ├─ 非 2xx   日志 resp= body_preview       错误 body  body_text（不转义，给调用方解析）
  ├─ 解码失败 日志 resp= body_preview
  └─ 成功     日志 resp= FromBody::resp_preview 有值（Json<T> 打码、String 原文）→ to_one_line(..)
                         没有（Bytes / ()）                                     → 不输出 resp 字段

body_preview = to_one_line ∘ body_text
body_text    = is_text_content_type ? decode_charset : "<N bytes ..>"
```

规则：给人看的（日志 `req=` / `resp=`）先打码、再转文本、最后 `to_one_line`；成功响应只有类型能打码。

## 4. debug 日志（`debug-log` feature）

日志的格式见模块文档的「debug 日志」，这里是实现：

| 内容 | 说明 |
|---|---|
| `ReqwestConfig::debug`、`ReqwestClient::debug`、`ReqwestClient::set_debug`、`ReqwestClient::set_pretty` | 对外的开关 |
| `DebugState` | client 级的状态（默认头、两个开关），所有 clone 共享 |
| `DebugDraft` → `DebugCtx` | 一次请求：发出前取 params / body 原文，build 出请求后补上 url 和请求头 |
| `DebugStart` / `DebugEnd` | 两条日志的 JSON 结构：start 是请求，结束是请求加响应 |
| `headers_json` / `text_json` | 头转成 JSON 对象；文本能解析成 JSON 就嵌套，否则是字符串 |

- 整个 `debug.rs` 只在这个 feature 下编译（`mod.rs` 里 `#[cfg(feature = "debug-log")] mod debug;`），模块里不再写 cfg；
  别的文件只在字段和调用处有 cfg
- 接进发送流程：要用的东西在 `dispatch` 里算好放进 `SendCtx::debug`，各出口先调 `debug_start` / `debug_end`，
  返回 false（不是 debug）才打原来的日志
- 序列化失败返回 `JsonError`，一路传给 `send`
