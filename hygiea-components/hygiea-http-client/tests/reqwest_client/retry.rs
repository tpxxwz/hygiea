//! 带重试的发送：`send((&client, RetryCtx))`。什么时候交给 Retry、交过去的是什么、次数怎么算、
//! Retry 改了配置后下一次是否生效

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hygiea_http_client::reqwest_client::*;
use serde::{Deserialize, Serialize};

use crate::support::*;

/// 计数的服务：第 n 次请求（从 1 开始）交给 `handler`，返回地址和计数器
async fn counted<F>(handler: F) -> (String, Arc<AtomicUsize>)
where
    F: Fn(usize, &Received) -> Reply + Send + Sync + 'static,
{
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let base = serve(move |req| handler(counter.fetch_add(1, Ordering::SeqCst) + 1, req)).await;
    (base, count)
}

/// 记录每次被调用时看到的东西，并一直要求重试。clone 一份交给 RetryCtx，自己留一份看记录
#[derive(Clone, Default)]
struct Recorder(Arc<std::sync::Mutex<Vec<(usize, String)>>>);

impl Recorder {
    fn attempts(&self) -> Vec<usize> {
        self.0.lock().unwrap().iter().map(|(n, _)| *n).collect()
    }

    fn stages(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(_, s)| s.clone())
            .collect()
    }
}

/// 记录后一律按 Unknown 处理：原样再发，直到次数用完
impl<Params: Send, Req: Send> Retry<Params, Req> for Recorder {
    async fn retry(
        &mut self,
        attempt: usize,
        cfg: RequestConfig<Params, Req>,
        failure: SendFailure,
    ) -> RetryDecision<Params, Req> {
        let stage = match failure.stage {
            FailStage::Transport { .. } => "transport".into(),
            FailStage::Status(resp) => format!("status {}", resp.status.as_u16()),
            FailStage::Decode(_) => "decode".into(),
        };
        self.0.lock().unwrap().push((attempt, stage));
        RetryDecision::Unknown(cfg)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
struct Item {
    id: u32,
}

/// 次数：拿到结果就停，用完次数返回最后一次的错误
mod attempts {
    use super::*;

    /// 前两次 500，第三次成功：发 3 次，Retry 被问 2 次，返回成功的结果
    #[tokio::test]
    async fn succeeds_after_failures() {
        let (base, count) = counted(|n, _| match n {
            1 | 2 => Reply::json(500, "{}"),
            _ => Reply::json(200, r#"{"id":7}"#),
        })
        .await;
        let recorder = Recorder::default();
        let resp = RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Json<Item>>((
                &local_config().build().unwrap(),
                RetryCtx::new(5, recorder.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.body, Item { id: 7 });
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert_eq!(recorder.attempts(), [1, 2]);
    }

    /// 一直失败：最多发 1 + max_retries 次，返回最后一次的错误；用完之后不再问 Retry
    #[tokio::test]
    async fn exhausted_returns_last_error() {
        let (base, count) = counted(|n, _| Reply::json(500, format!(r#"{{"n":{n}}}"#))).await;
        let recorder = Recorder::default();
        let err = RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(2, recorder.clone()),
            ))
            .await
            .unwrap_err();
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert_eq!(recorder.attempts(), [1, 2]);
        assert!(err.is(HttpClientErr::NonSuccessStatus));
        assert_eq!(err.err_args()["body"], r#"{"n":3}"#);
    }

    /// max_retries = 0：只发一次，不问 Retry
    #[tokio::test]
    async fn zero_retries_sends_once() {
        let (base, count) = counted(|_, _| Reply::json(500, "{}")).await;
        let recorder = Recorder::default();
        let err = RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(0, recorder.clone()),
            ))
            .await
            .unwrap_err();
        assert!(err.is(HttpClientErr::NonSuccessStatus));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(recorder.attempts().is_empty());
    }

    /// 第一次就成功：不问 Retry
    #[tokio::test]
    async fn success_does_not_call_retry() {
        let (base, count) = counted(|_, _| Reply::json(200, r#"{"id":1}"#)).await;
        let recorder = Recorder::default();
        RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Json<Item>>((
                &local_config().build().unwrap(),
                RetryCtx::new(3, recorder.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(recorder.attempts().is_empty());
    }
}

/// 交给 Retry 的是什么：三种阶段各自带的信息
mod stages {
    use super::*;

    /// 非 2xx：Status 里是原始的 status、headers、body 字节
    #[tokio::test]
    async fn status_carries_raw_response() {
        let (base, _) = counted(|n, _| match n {
            1 => Reply::json(503, r#"{"busy":true}"#).header("retry-after", "0"),
            _ => Reply::json(200, "{}"),
        })
        .await;
        let seen = Arc::new(std::sync::Mutex::new(None));
        let record = seen.clone();
        RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(1, move |_: usize, cfg: RequestConfig, f: SendFailure| {
                    let record = record.clone();
                    async move {
                        let FailStage::Status(resp) = f.stage else {
                            return RetryDecision::Stop(f.err);
                        };
                        *record.lock().unwrap() = Some((
                            resp.status.as_u16(),
                            resp.header("retry-after").map(str::to_owned),
                            resp.body,
                        ));
                        RetryDecision::Retry(cfg)
                    }
                }),
            ))
            .await
            .unwrap();
        let (status, retry_after, body) = seen.lock().unwrap().take().unwrap();
        assert_eq!(status, 503);
        assert_eq!(retry_after.as_deref(), Some("0"));
        assert_eq!(body, &br#"{"busy":true}"#[..]);
    }

    /// 2xx 但解码失败：Decode 里是原始响应，err 是 DecodeFailed。
    /// 用来处理 200 + 业务失败这类「不是想要的结果」
    #[tokio::test]
    async fn decode_failure_is_retried_with_raw_body() {
        let (base, count) = counted(|n, _| match n {
            1 => Reply::json(200, r#"{"id":"not a number"}"#),
            _ => Reply::json(200, r#"{"id":2}"#),
        })
        .await;
        let seen = Arc::new(std::sync::Mutex::new(None));
        let record = seen.clone();
        let resp = RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Json<Item>>((
                &local_config().build().unwrap(),
                RetryCtx::new(1, move |_: usize, cfg: RequestConfig, f: SendFailure| {
                    let record = record.clone();
                    async move {
                        assert!(f.err.is(HttpClientErr::DecodeFailed), "{:#}", f.err);
                        let FailStage::Decode(resp) = f.stage else {
                            return RetryDecision::Stop(f.err);
                        };
                        *record.lock().unwrap() = Some(resp.body);
                        RetryDecision::Retry(cfg)
                    }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.body, Item { id: 2 });
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let body = seen.lock().unwrap().take().unwrap();
        assert_eq!(body, &br#"{"id":"not a number"}"#[..]);
    }

    /// 连不上：Transport，connect 为 true，没有 status
    #[tokio::test]
    async fn connect_failure_is_transport() {
        let base = closed_port_url().await;
        let seen = Arc::new(std::sync::Mutex::new(None));
        let record = seen.clone();
        let err = RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(1, move |_: usize, _: RequestConfig, f: SendFailure| {
                    let record = record.clone();
                    async move {
                        if let FailStage::Transport {
                            timeout,
                            connect,
                            status,
                        } = f.stage
                        {
                            *record.lock().unwrap() = Some((timeout, connect, status));
                        }
                        RetryDecision::Stop(f.err)
                    }
                }),
            ))
            .await
            .unwrap_err();
        assert!(err.is(HttpClientErr::RequestFailed));
        assert_eq!(*seen.lock().unwrap(), Some((false, true, None)));
    }

    /// 超时：Transport，timeout 为 true
    #[tokio::test]
    async fn timeout_is_transport() {
        let (base, count) = counted(|n, _| match n {
            1 => Reply::json(200, "{}").delay(std::time::Duration::from_secs(2)),
            _ => Reply::json(200, "{}"),
        })
        .await;
        let recorder = Recorder::default();
        RequestConfig::plain(Method::GET, format!("{base}/x"))
            .timeout(std::time::Duration::from_millis(100))
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(1, recorder.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(recorder.stages(), ["transport"]);
    }
}

/// Retry 自己决定：放弃、改配置
mod decisions {
    use super::*;

    /// Stop：不管还剩几次都立刻停下，send 返回这个错误
    #[tokio::test]
    async fn giving_up_returns_its_error() {
        let (base, count) = counted(|_, _| Reply::json(500, "{}")).await;
        let err = RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(5, |_: usize, _: RequestConfig, f: SendFailure| async move {
                    RetryDecision::Stop(f.err)
                }),
            ))
            .await
            .unwrap_err();
        assert!(err.is(HttpClientErr::NonSuccessStatus));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// 按错误改配置：401 时从响应里拿到新 token，换上再发，下一次按改过的发
    #[tokio::test]
    async fn config_adjusted_from_error_is_used_next_time() {
        let (base, _) = counted(|_, req| {
            let token = req.headers.iter().find(|(k, _)| k == "x-token");
            match token.map(|(_, v)| v.as_str()) {
                Some("fresh") => Reply::json(200, "{}"),
                _ => Reply::json(401, r#"{"new_token":"fresh"}"#),
            }
        })
        .await;
        let resp = RequestConfig::plain(Method::GET, format!("{base}/x"))
            .headers(header_map(&[("x-token", "stale")]))
            .unwrap()
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(
                    1,
                    |_: usize, cfg: RequestConfig, f: SendFailure| async move {
                        let FailStage::Status(resp) = &f.stage else {
                            return RetryDecision::Stop(f.err);
                        };
                        if resp.status != 401 {
                            return RetryDecision::Stop(f.err);
                        }
                        let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
                        let token = body["new_token"].as_str().unwrap();
                        match cfg.headers(header_map(&[("x-token", token)])) {
                            Ok(cfg) => RetryDecision::Retry(cfg),
                            Err(e) => RetryDecision::Stop(e),
                        }
                    },
                ),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
    }

    /// Retry 和 Unknown 共用 max_retries 的次数：交替返回也只发 1 + max_retries 次
    #[tokio::test]
    async fn retry_and_unknown_share_the_limit() {
        let (base, count) = counted(|_, _| Reply::json(500, "{}")).await;
        let err = RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(
                    3,
                    |n: usize, cfg: RequestConfig, _: SendFailure| async move {
                        if n % 2 == 1 {
                            RetryDecision::Retry(cfg)
                        } else {
                            RetryDecision::Unknown(cfg)
                        }
                    },
                ),
            ))
            .await
            .unwrap_err();
        assert!(err.is(HttpClientErr::NonSuccessStatus));
        assert_eq!(count.load(Ordering::SeqCst), 4);
    }

    /// 发出之前的错误不交给 Retry，也不发请求
    #[tokio::test]
    async fn before_send_error_is_returned_directly() {
        let recorder = Recorder::default();
        let err = RequestConfig::plain(Method::GET, "not a url")
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(3, recorder.clone()),
            ))
            .await
            .unwrap_err();
        assert!(err.is(HttpClientErr::InvalidUrl));
        assert!(recorder.attempts().is_empty());
    }
}

/// 可重发的 body：每次重试发出去的内容一样
mod bodies {
    use super::*;

    /// 连续两次失败后成功，服务端每次收到的 body 都一样
    async fn bodies_seen<B: IntoBody + Clone + Send>(body: B) -> Vec<String> {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        let (base, _) = counted(move |n, req| {
            record
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&req.body).into_owned());
            if n < 3 {
                Reply::json(500, "{}")
            } else {
                Reply::json(200, "{}")
            }
        })
        .await;
        let recorder = Recorder::default();
        RequestConfig::with_body(Method::POST, format!("{base}/x"), body)
            .send::<Bytes>((
                &local_config().build().unwrap(),
                RetryCtx::new(2, recorder.clone()),
            ))
            .await
            .unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 3);
        seen
    }

    #[tokio::test]
    async fn json_body_is_resent() {
        let seen = bodies_seen(Json(Item { id: 1 })).await;
        assert!(seen.iter().all(|b| b == r#"{"id":1}"#), "{seen:?}");
    }

    #[tokio::test]
    async fn raw_body_is_resent() {
        let seen = bodies_seen(Raw::new(ContentType::Csv, "a,b")).await;
        assert!(seen.iter().all(|b| b == "a,b"), "{seen:?}");
    }

    /// Multipart：每次重新生成 boundary，但各段内容一样
    #[tokio::test]
    async fn multipart_body_is_resent() {
        let body = Multipart::new().text("user", "alice").part(
            "avatar",
            MultipartPart::bytes(&b"PNG"[..])
                .file_name("a.png")
                .mime("image/png"),
        );
        let seen = bodies_seen(body).await;
        for b in &seen {
            assert!(b.contains("name=\"user\"\r\n\r\nalice"), "{b}");
            assert!(b.contains("filename=\"a.png\""), "{b}");
            assert!(b.contains("Content-Type: image/png\r\n\r\nPNG"), "{b}");
        }
    }
}
