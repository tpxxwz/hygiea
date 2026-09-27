//! 重试：[`RetryCtx`] 带着次数上限和重试判断 [`Retry`]，作为 [`RequestConfig::send`] 的第二个参数传进去。
//!
//! ```ignore
//! cfg.send::<Json<User>>(&client).await?;                              // 不重试
//! cfg.send::<Json<User>>((&client, RetryCtx::new(3, policy))).await?;  // 最多重试 3 次
//! ```
//!
//! 流程：发一次，拿到解码成功的 `Resp` 就返回；请求发出去之后失败（传输失败、非 2xx、解码失败）就把
//! 这次的配置和 [`SendFailure`] 交给 [`Retry::retry`]，按它返回的 [`RetryDecision`] 再发或者停下。
//! 重试次数用完后不再问 `Retry`，返回最后一次的错误。发出之前的错误（URL、params、header、body 构造）
//! 重试也没用，直接返回，不交给 `Retry`。
//!
//! # 什么请求可以重试
//!
//! 重试就是把请求再发一遍，而失败不代表对方没处理：超时、读 body 中断时，请求可能已经到了服务端并执行了，
//! 只是响应没回来。所以只在两种情况下重试：
//!
//! - 对方接口幂等：GET / PUT / DELETE，或者请求里带了对方认的幂等键，重复执行结果一样；
//! - 根据错误调整请求后再发：401 换 token、签名过期换时间戳重签、换一个新的幂等键……
//!
//! 不幂等的请求（比如下单、转账的 POST）遇到 [`FailStage::Transport`] 时不要原样重发，
//! 不知道对方是否已经执行过；这种情况返回 [`RetryDecision::Stop`]，交给业务查询结果后再决定。
//!
//! # 要求
//!
//! params 和 body 是 `Clone`（每次发之前留一份，发出去的那份被消费掉；流式 body 不是 `Clone`，
//! 开不了重试），响应类型实现 [`FromBytes`]（body 整个读进内存，失败时原始响应才留得住）

use std::future::Future;

use hygiea_core::HyErr;

use super::Client;
use super::body::IntoBody;
use super::request::{HttpResponse, RequestConfig};
use super::response::FromBytes;
use super::send::{IntoSender, SendFailure};

/// [`Retry::retry`] 的判断结果。`Retry` 和 `Unknown` 行为相同、共用 [`RetryCtx::max_retries`] 的次数，
/// 区别只在写法上表明判断依据，读代码的人一眼能看出哪些错误是确认过可以重试的
pub enum RetryDecision<Params, Req> {
    /// 确认可以重试：用这个配置再发（可以是按错误改过的）
    Retry(RequestConfig<Params, Req>),
    /// 立即结束，不管还剩几次，`send` 返回这个错误（一般就是 `failure.err`）
    Stop(HyErr),
    /// 判断不了是不是能恢复：原样再发，直到次数用完
    Unknown(RequestConfig<Params, Req>),
}

/// [`Retry::retry`] 返回的 future：输出 [`RetryDecision`]，并且是 `Send`。
/// 所有满足条件的 future 自动实现它，`async fn` / `async move {}` 直接就是
pub trait RetryFuture<Params, Req>: Future<Output = RetryDecision<Params, Req>> + Send {}

impl<Params, Req, F> RetryFuture<Params, Req> for F where
    F: Future<Output = RetryDecision<Params, Req>> + Send
{
}

/// 重试判断：每次发送失败后调用，返回 [`RetryDecision`]。
///
/// - `attempt`：刚失败的是第几次发送，从 1 开始
/// - `cfg`：刚才那次发送用的配置。可以按 `failure` 里的信息改完再交回去：`failure.err.err_args()` 里有
///   `method`、`url`、`status`、`body`，[`FailStage::Status`](super::FailStage::Status) / [`FailStage::Decode`](super::FailStage::Decode) 里有原始的 headers 和字节
/// - 要退避就在返回前 sleep
///
/// 自己的类型实现时直接写 `async fn retry(..) -> RetryDecision<Params, Req>`。
/// 闭包也行：`FnMut(usize, RequestConfig<Params, Req>, SendFailure) -> impl RetryFuture<Params, Req>`，
/// 闭包的参数类型要写出来，编译器没法从这个 trait 反推
pub trait Retry<Params, Req>: Send {
    fn retry(
        &mut self,
        attempt: usize,
        cfg: RequestConfig<Params, Req>,
        failure: SendFailure,
    ) -> impl RetryFuture<Params, Req>;
}

impl<Params, Req, F, Fut> Retry<Params, Req> for F
where
    F: FnMut(usize, RequestConfig<Params, Req>, SendFailure) -> Fut + Send,
    Fut: RetryFuture<Params, Req>,
{
    fn retry(
        &mut self,
        attempt: usize,
        cfg: RequestConfig<Params, Req>,
        failure: SendFailure,
    ) -> impl RetryFuture<Params, Req> {
        self(attempt, cfg, failure)
    }
}

/// 一次带重试的发送：最多发 `1 + max_retries` 次（`Retry` 和 `Unknown` 都算），每次失败怎么办由 `retry` 决定。
/// 每次 `send` 现场构造，次数和策略可以按请求不同
#[derive(Debug, Clone)]
pub struct RetryCtx<T> {
    pub max_retries: usize,
    pub retry: T,
}

impl<T> RetryCtx<T> {
    pub fn new(max_retries: usize, retry: T) -> Self {
        Self { max_retries, retry }
    }
}

impl<T> super::send::sealed::Sealed for (&Client, RetryCtx<T>) {}

impl<Params, Req, Resp, T> IntoSender<Params, Req, Resp> for (&Client, RetryCtx<T>)
where
    Params: serde::Serialize + Clone + Send,
    Req: IntoBody + Clone + Send,
    Resp: FromBytes + Send,
    T: Retry<Params, Req>,
{
    fn send_request(
        self,
        cfg: RequestConfig<Params, Req>,
    ) -> impl Future<Output = Result<HttpResponse<Resp>, HyErr>> + Send {
        let (
            client,
            RetryCtx {
                max_retries,
                mut retry,
            },
        ) = self;
        async move {
            let mut cfg = cfg;
            let mut attempt = 0;
            loop {
                attempt += 1;
                let kept = cfg.clone();
                // 外层 ? 是发出之前的错误：直接返回，不交给 retry
                let failure = match cfg.send_once_bytes::<Resp>(client).await? {
                    Ok(resp) => return Ok(resp),
                    Err(failure) => failure,
                };
                if attempt > max_retries {
                    return Err(failure.err);
                }
                cfg = match retry.retry(attempt, kept, failure).await {
                    RetryDecision::Retry(cfg) | RetryDecision::Unknown(cfg) => cfg,
                    RetryDecision::Stop(err) => return Err(err),
                };
            }
        }
    }
}
