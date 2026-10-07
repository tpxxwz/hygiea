# HyErr 错误体系待办

## 设计前提（不改）

hygiea 的目标是企业级服务端架构：一套系统对外输出的错误码必须全局唯一、含义稳定——调用方和前端按 code 分支，
监控告警按 code 聚合，排查时拿到 code 就能定位是哪个错误。两个 crate 撞码会让这些静默出错。

所以这几点是有意的设计：

- 8 位错误码 = 项目前缀 + 模块前缀 + 业务码，跨 crate 全局唯一
- core 的 ctor 在进程启动时收集所有 crate 注册的错误码（`linkme` distributed slice），重复就 `exit(1)`：
  fail-fast，带着撞码的服务不能上线。放在 ctor 里而不是 `Registry` 启动时，是为了不走 Registry 的进程也逃不掉检查
- 框架内置错误用项目前缀 `999`，对外统一换成 `SysErr`

下面的待办都在这个前提之内。

## 待办

### 类型化地判断错误

现在判断错误只能 `err.is(Kind::X)` 一个个比，取字段只能 `err_args()["status"]`（JSON 值），编译器帮不上忙。

- 做法：`#[derive(hy_err)]` 给每个 enum 额外生成 `fn from_err_code(code: &str) -> Option<Self>`（编译期写好的 `match`，
  不查表），HyErr 加 `err.kind::<E>() -> Option<E>`，内部就是 `E::from_err_code(self.err_code())`。拿到 enum 后 `match`：
  ```rust
  match err.kind::<HttpClientErr>() {
      Some(HttpClientErr::NonSuccessStatus) => …,
      Some(HttpClientErr::RequestFailed) => …,
      _ => …,
  }
  ```
- 不需要全局 enum 也不需要 ctor：调用方 `match` 时想的本来就是某一个 enum
- 限制：变体不带字段，参数在 `err_args`（JSON）里，所以只解决「是哪个错误」，字段仍然从 `err_args()` 取。
  字段也要有类型的话，变体得带数据（比如 `NonSuccessStatus { status: u16, .. }`），要改宏和错误的构造方式，是更大的改动

为什么不做跨 crate 的全局 enum：过程宏按 crate 各自展开，互相看不见，编译顺序也不保证；Rust 的 enum 只能在一个 crate 里定义完，
别的 crate 加不了变体。只能在最终二进制里用 `build.rs` 生成，但 `build.rs` 拿不到依赖的宏展开结果，只能解析源码 / metadata，
很脆弱，生成的 enum 也只在那个二进制里，库代码用不了。

### 错误码目录（运行时）

ctor 已经收集了所有 crate 的注册（`ERR_REGISTRATIONS`：码 + 模板），可以顺带建一个运行时的目录。它解决不了 `match`
（`match` 要编译期的类型），但企业级服务用得上：

- 按 code 查它属于哪个 crate、哪个 enum、哪个变体、模板是什么
- 导出全量错误码表，给文档、前端、i18n 用；或者做个管理接口列出系统的全部错误码
- 做法：`ErrRegistration` 加上 crate 名、enum 名、变体名（宏里都拿得到），提供 `hygiea::error::catalog()` 之类的只读访问

## 视情况再定

### 组件要不要能脱离 hygiea 单独用

如果以后打算把组件（比如 `hygiea-http-client`）提供给不用 hygiea 体系的人，他们会被迫接受 HyErr 和全局错误码。
那时的做法：组件定义自己的类型化错误（比如 `HttpError { kind, method, url, status, body }`，实现 `std::error::Error`），
在 feature 下提供 `impl From<HttpError> for HyErr`，hygiea 用户 `?` 一下照样拿到错误码。

- 代价：组件的 `error.rs` 重写、所有构造错误的地方改，`SendFailure.err`、`RetryDecision::Stop` 的类型跟着变
- 只服务于 hygiea 架构的话不需要做

## 考虑过、不采用

- 把重复码检查从 ctor 挪到 `Registry` 启动时、改成返回 `Err`：不走 Registry 的进程就发现不了撞码，削弱了全局唯一的保证
- 不再默认项目前缀 `"000"`、没配前缀就编译报错：撞码要同时满足「两个 crate 都用 hy_err 定义错误、都没配前缀、业务码重合、链接进同一个进程」，
  而 hygiea 本身提供完整能力，一般不需要基于它再做二次开发的库；真做二次开发也会先了解错误码设计、配好前缀。
  为这个少见的情况让每个定义错误的人都先配前缀，增加的心智负担不划算
- 不用 app 时把重复码的 `exit(1)` 降级成警告（`#[cfg(feature = "app")]` 才退出）：针对的是同一个少见场景
  （不用 app 的库各自定义错误、都落到默认前缀、业务码重合），不值得让检查行为随 feature 变化
- 错误码唯一性只限定在 crate 内（身份改成「crate 名 + 码」）：放弃了「全系统一个码只有一个含义」，和设计前提冲突
