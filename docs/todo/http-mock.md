# HTTP mock（hygiea-test）待办

给使用方写第 2 层测试用的 mock HTTP 服务：本地起服务，client 的 base_url 指过来，按 cassette 返回响应。
实现在 `hygiea-test` 的 `http_mock`（feature `http-mock`），测试和示例资源在 `hygiea-test/tests/`。
最早的原型在 `playground/tests/mock_server/`，已经被 hygiea-test 取代，只作参考。

## 已经定下来的

- 底层用 httpmock：内置匹配器多，还在维护。不用 wiremock
- cassette 用自定义的 TOML 格式，翻译成 httpmock 的代码 API。动态逻辑写 Rhai 脚本（纯 Rust，不依赖 C）
- **不支持 httpmock 原生 YAML，不做录制**：录下来的只是某一次请求的快照，参数一变、状态一变就对不上，
  不如手写 Rhai 逻辑
- 测试分层：
  - 第 2 层 server：`<项目>/server/` 模拟整个服务，一个 server 一份共用的 state（相当于数据库），
    **必须符合使用方的 DTO**
  - 第 2 层 cases：`<项目>/cases/` 单接口用例，边界情况、故意构造的错误响应；可以有自己的 state
  - 第 3 层：live 测试连真实服务，由它保证 DTO 和真实服务一致
- 目录按项目分：`<项目>/{server,cases}/`，通用的放 `common/`。cassette 里的 `script`、`state_file`、
  `body_file` 和脚本里读文件的路径都相对 cassette 所在的目录
- state 一个 server 一份，不同 server 互不影响；只在内存里，不写回文件
- state 的初始值只有一个来源：`Cassette::db("xxx.json")`。cassette 里不写 state
  （原来的 `[state]`、`state_file`、`state_from`、`with_state`、`with_db` 都去掉了）
- 使用方不为了测试改线上代码：辅助函数都写在测试代码里
- **不做接口约定校验、不分 server / case 模式、db 不带类型参数**：mock 的响应和数据都是使用方自己写的，
  校验自己写的东西意义不大；和真实服务是否一致由第 3 层保证。API 只剩 `Cassette::load(root, &[..]).db(json).start()`

## playground 原型里做过的（都已带进 hygiea-test）

- TOML 加载：`response` / `responses`（按调用次数）/ `script` 三选一；`body_file`、`{{base}}`
- 匹配：`method`、`path`（支持 `{名字}` 占位符）、`path_prefix`、`query`、`headers`、`json_body`、
  `json_body_includes`、`body_includes`
- Rhai：`request`（query / query_all / header / param / text / json / form …）、`state`、`calls`、`base`、
  `read_json` / `read_text`、`import "common"`
- state：`[state]`、`state_file`；`mock.state::<T>()`、`mock.update_state(..)`
- `start_all`：多个 cassette 挂到一个 server，先注册的优先，state 键重复报错
- 示例：moji、momo/markji 的 server 和 cases，common 的 echo

## 步骤

### 1. 去掉原生 YAML 和录制（已完成）

- `playground/Cargo.toml` 去掉 httpmock 的 `record` feature
- 删 `momo/markji/cases/list_folders.yaml` 和对应测试；`cassette.rs` 去掉 YAML 分支，只认 `.toml`

### 2. 接口约定校验（做过，后来整个删掉了，见上面「已经定下来的」）

- 注册：`.contract::<Resp, Err>(Method::GET, "/decks/{deck}")`，方法用 `http::Method`；路径模板和 cassette 同一套写法
- 每次响应时按**实际请求路径**找约定：2xx 用 `Resp` 反序列化，其他状态码用 `Err`
- 多个约定都能匹配时：不带占位符的优先，其次固定段多的，再按注册顺序
- 二进制 / 非 JSON 的成功响应：`.contract_raw(Method::GET, "/tts/{voice}/{id}.mp3")`，只查状态码
- 校验失败不 panic，记下接口、第几次调用、serde 错误、实际 body；`mock.assert_valid()` 统一报
- 静态响应和脚本响应都校验；cassette 里 `violates_contract = true` 的跳过

### 3. server / case 两个入口（做过，后来删掉了；server / cases 只作为目录约定）

- `Cassette::server(&[..])`：只能加载 `*/server/` 下的；每个请求都必须命中约定并通过校验；不允许 `violates_contract`
- `Cassette::case(name)`：只能加载 `*/cases/` 下的；有约定的才校验；允许 `violates_contract`
- case 的 state 两种来源都支持，可以同时用：
  - 自定义：cassette 里写 `[state]` / `state_file`，数据不必和 server 对得上
  - 读 server 的：`.state_from("moji/server/db")`，加载 server 那份数据库
  - 测试里再用 `update_state` 改成需要的前提
- case 可以引用 server 的脚本（`script = "../server/xxx.rhai"`）

### 4. 补全匹配和响应能力（已完成，`delay_ms` 只支持写在 interaction 上，脚本里不能动态指定）

- TOML 的匹配字段和 httpmock 的方法同名（`header_exists`、`query_param_matches`、`cookie`、
  `json_body_excludes`、`form_urlencoded_tuple` …），按参数形状分几类用宏生成映射
- 响应支持 `delay_ms`（静态和脚本都行），测超时用
- `match_script`：Rhai 返回 true / false 决定是否匹配，对应 httpmock 的 `is_true`
- `request.cookie(name)`

### 5. 挪进 hygiea-test（已完成）

- 模块 `hygiea_test::http_mock`，放在 `http-mock` feature 后面；导出的 wiremock 换成 httpmock
- 依赖：httpmock、rhai（`sync`、`serde`）、toml、regex、encoding_rs、form_urlencoded，都跟着 feature
- cassette 根目录由调用方给（`.root(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resources/httpmock"))`
  或者一个宏），库里的 `CARGO_MANIFEST_DIR` 是 hygiea-test 自己的
- `http-client` feature：`http_client()` 返回测试用的 reqwest client
- 同步 `docs/architecture.md`、README 的 crate 表

### 6. dict-jp 接入（进行中）

- `[dev-dependencies] hygiea-test = { .., features = ["http-mock", "log"] }`
- 测试写在各 client 文件末尾的 `#[cfg(test)] mod mock_tests` / `mod live_tests`；
  构造 client 的函数（线上的 client，只把 base_url 指到 mock）
- 资源放 `server/tests/resources/httpmock/`，从 playground 搬 moji、momo/markji
- 先做 markji（base_url 已经能配）：cases 测异常响应，server 测完整调用链
- moji 等 web / api 地址改成可配置之后再接（这一步由使用方自己改）

## 还没定的

- 以后要不要把 httpmock 换成基于 axum 自己实现
