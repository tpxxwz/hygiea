//! `send`：请求发了什么、响应怎么返回、各类失败报什么错

use std::error::Error as _;

use hygiea_core::BaseErr;
use hygiea_core::net::http::*;

use crate::support::*;

/// 发出去的请求内容：以回显服务看到的为准
mod request_content {
    use super::*;

    async fn seen<P: serde::Serialize, B: IntoBody>(cfg: RequestConfig<P, B>) -> serde_json::Value {
        cfg.send::<Json<serde_json::Value>>(&local_config().build().unwrap())
            .await
            .unwrap()
            .body
            .0
    }

    /// 方法和路径
    #[tokio::test]
    async fn method_and_path() {
        let base = serve(echo).await;
        let seen = seen(RequestConfig::plain(
            Method::DELETE,
            format!("{base}/items/1"),
        ))
        .await;
        assert_eq!(seen["method"], "DELETE");
        assert_eq!(seen["path"], "/items/1");
    }

    /// params 拼到 query 上，发的是原文（日志里才打码）
    #[tokio::test]
    async fn params_become_query() {
        let base = serve(echo).await;
        let seen = seen(RequestConfig::with_params(
            Method::GET,
            format!("{base}/q?x=0"),
            login("t-1"),
        ))
        .await;
        assert_eq!(seen["query"], "x=0&user=alice&token=t-1");
    }

    /// Json body：content-type 和原文
    #[tokio::test]
    async fn json_body() {
        let base = serve(echo).await;
        let seen = seen(RequestConfig::with_body(
            Method::POST,
            format!("{base}/j"),
            Json(login("t-1")),
        ))
        .await;
        assert_eq!(seen["headers"]["content-type"], "application/json");
        assert_eq!(seen["body"], r#"{"user":"alice","token":"t-1"}"#);
    }

    /// 临时文件，测试结束删掉
    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(name: &str, content: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!("hygiea-{}-{name}", std::process::id()));
            std::fs::write(&path, content).unwrap();
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// 流式上传（Body::wrap_stream）：不知道长度，按 chunked 发，服务端收到的就是各块拼起来的原文
    #[tokio::test]
    async fn raw_stream_upload() {
        let base = serve(echo).await;
        let chunks = ["hello ", "stream ", "upload"].map(|c| Ok::<_, std::io::Error>(c));
        let body = Body::wrap_stream(futures_util::stream::iter(chunks));
        let seen = seen(RequestConfig::with_body(
            Method::PUT,
            format!("{base}/up"),
            Raw(ContentType::OctetStream, body),
        ))
        .await;
        assert_eq!(seen["headers"]["content-type"], "application/octet-stream");
        assert_eq!(seen["headers"]["transfer-encoding"], "chunked");
        assert_eq!(seen["body"], "hello stream upload");
    }

    /// 文件流式上传：tokio::fs::File 直接转成 Body，不整个读进内存
    #[tokio::test]
    async fn file_upload() {
        let base = serve(echo).await;
        let content = "line\n".repeat(20_000);
        let file = TempFile::new("upload.txt", content.as_bytes());
        let body = Body::from(tokio::fs::File::open(&file.0).await.unwrap());
        let seen = seen(RequestConfig::with_body(
            Method::PUT,
            format!("{base}/file"),
            Raw(ContentType::Plain, body),
        ))
        .await;
        assert_eq!(seen["headers"]["content-type"], "text/plain");
        assert_eq!(seen["body"], content);
    }

    /// multipart 里带一个文件 part：content-type 带 boundary，body 里有字段、文件名和文件内容
    #[tokio::test]
    async fn multipart_file_upload() {
        let base = serve(echo).await;
        let file = TempFile::new("report.csv", b"a,b\n1,2\n");
        let part =
            multipart::Part::stream(Body::from(tokio::fs::File::open(&file.0).await.unwrap()))
                .file_name("report.csv")
                .mime_str("text/csv")
                .unwrap();
        let form = multipart::Form::new()
            .text("kind", "daily")
            .part("file", part);
        let seen = seen(RequestConfig::with_body(
            Method::POST,
            format!("{base}/mp"),
            form,
        ))
        .await;
        let content_type = seen["headers"]["content-type"].as_str().unwrap();
        assert!(
            content_type.starts_with("multipart/form-data; boundary="),
            "{content_type}"
        );
        let body = seen["body"].as_str().unwrap();
        assert!(
            body.contains(r#"name="kind""#) && body.contains("daily"),
            "{body}"
        );
        assert!(body.contains(r#"filename="report.csv""#), "{body}");
        assert!(body.contains("Content-Type: text/csv"), "{body}");
        assert!(body.contains("a,b\n1,2\n"), "{body}");
    }

    /// Form body：urlencoded
    #[tokio::test]
    async fn form_body() {
        let base = serve(echo).await;
        let seen = seen(RequestConfig::with_body(
            Method::POST,
            format!("{base}/f"),
            Form(login("t 1")),
        ))
        .await;
        assert_eq!(
            seen["headers"]["content-type"],
            "application/x-www-form-urlencoded"
        );
        assert_eq!(seen["body"], "user=alice&token=t+1");
    }

    /// Raw body：声明的 content-type 和原样的字节
    #[tokio::test]
    async fn raw_body() {
        let base = serve(echo).await;
        let seen = seen(RequestConfig::with_body(
            Method::POST,
            format!("{base}/r"),
            Raw::new(ContentType::Csv, "a,b\n1,2"),
        ))
        .await;
        assert_eq!(seen["headers"]["content-type"], "text/csv");
        assert_eq!(seen["body"], "a,b\n1,2");
    }

    /// 认证头
    #[tokio::test]
    async fn auth_header() {
        let base = serve(echo).await;
        let seen = seen(
            RequestConfig::plain(Method::GET, format!("{base}/a")).auth(Auth::Bearer("tok".into())),
        )
        .await;
        assert_eq!(seen["headers"]["authorization"], "Bearer tok");
    }
}

/// 拿到响应之后：元信息、按状态码解码或报错
mod responses {
    use super::*;

    const ROUTES: &[(&str, u16, &str)] = &[
        ("/ok", 200, r#"{"user":"alice","token":"t-1"}"#),
        ("/created", 201, r#"{"user":"bob","token":"t-2"}"#),
        ("/fail", 500, r#"{"error":"boom"}"#),
        ("/bad-shape", 200, r#"{"user":1}"#),
    ];

    /// BodyStream 不整个读进内存，一块块读完拼起来就是原文；成功日志只有状态、没有 resp
    #[tokio::test]
    async fn stream_reads_body_in_chunks() {
        let big = "x".repeat(300_000);
        let expected = big.clone();
        let base = serve(move |_| Reply::json(200, big.clone())).await;
        let (out, _guard) = capture();
        let mut r = RequestConfig::plain(Method::GET, format!("{base}/big"))
            .send::<BodyStream>(&local_config().build().unwrap())
            .await
            .unwrap();
        assert_eq!(r.status, StatusCode::OK);
        let mut got = Vec::new();
        while let Some(chunk) = r.body.next().await {
            got.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(got, expected.as_bytes());
        let log = out.text();
        assert!(log.contains("http call success"), "{log}");
        assert!(!log.contains("resp="), "{log}");
    }

    /// BodyStream 写进文件：边收边写，写完的文件和原文一致，返回的字节数也对
    #[tokio::test]
    async fn stream_write_to_file() {
        let big = "0123456789".repeat(30_000);
        let expected = big.clone();
        let base = serve(move |_| Reply::json(200, big.clone())).await;
        let path = std::env::temp_dir().join(format!("hygiea-{}-download.bin", std::process::id()));
        let r = RequestConfig::plain(Method::GET, format!("{base}/file"))
            .send::<BodyStream>(&local_config().build().unwrap())
            .await
            .unwrap();
        let file = tokio::fs::File::create(&path).await.unwrap();
        let written = r.body.write_to(file).await.unwrap();
        let content = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(written, expected.len() as u64);
        assert_eq!(content, expected.as_bytes());
    }

    /// BodyStream 遇到非 2xx 和别的类型一样报 NonSuccessStatus，body 原文在 err_args 里
    #[tokio::test]
    async fn stream_non_2xx_is_status_error() {
        let base = serve_routes(ROUTES).await;
        let err = RequestConfig::plain(Method::GET, format!("{base}/fail"))
            .send::<BodyStream>(&local_config().build().unwrap())
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::NonSuccessStatus));
        assert_eq!(err.err_args["body"], r#"{"error":"boom"}"#);
    }

    /// 响应的元信息：状态、头、最终地址、耗时
    #[tokio::test]
    async fn response_metadata() {
        let base = serve(|req| echo(req).header("x-request-id", "rid-1")).await;
        let resp = RequestConfig::plain(Method::POST, format!("{base}/meta"))
            .send::<Bytes>(&local_config().build().unwrap())
            .await
            .unwrap();
        assert_eq!(resp.method, Method::POST);
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(resp.header("x-request-id"), Some("rid-1"));
        assert_eq!(resp.url.path(), "/meta");
        assert!(resp.elapsed > std::time::Duration::ZERO);
    }

    /// send 在 2xx 时解码，201 这类非 200 的 2xx 也算
    #[tokio::test]
    async fn send_decodes_any_2xx() {
        let base = serve_routes(ROUTES).await;
        let client = local_config().build().unwrap();
        let ok: HttpResponse<Json<Login>> = RequestConfig::plain(Method::GET, format!("{base}/ok"))
            .send(&client)
            .await
            .unwrap();
        assert_eq!(ok.body.0, login("t-1"));
        let created: HttpResponse<Json<Login>> =
            RequestConfig::plain(Method::GET, format!("{base}/created"))
                .send(&client)
                .await
                .unwrap();
        assert_eq!(created.status, StatusCode::CREATED);
        assert_eq!(created.body.0.user, "bob");
    }

    /// send 解码成 String / () / Bytes
    #[tokio::test]
    async fn send_other_body_types() {
        let base = serve_routes(ROUTES).await;
        let client = local_config().build().unwrap();
        let url = format!("{base}/ok");
        let s: HttpResponse<String> = RequestConfig::plain(Method::GET, &url)
            .send(&client)
            .await
            .unwrap();
        assert!(s.body.contains("alice"));
        let _: HttpResponse<()> = RequestConfig::plain(Method::GET, &url)
            .send(&client)
            .await
            .unwrap();
        let b: HttpResponse<Bytes> = RequestConfig::plain(Method::GET, &url)
            .send(&client)
            .await
            .unwrap();
        assert_eq!(b.body, s.body.as_bytes());
    }

    /// send 遇到非 2xx 报 BaseHttpErr::NonSuccessStatus，状态码、方法、地址、原文都在 err_args 里
    #[tokio::test]
    async fn send_non_2xx_is_status_error() {
        let base = serve_routes(ROUTES).await;
        let err = RequestConfig::plain(Method::GET, format!("{base}/fail"))
            .send::<Json<Login>>(&local_config().build().unwrap())
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::NonSuccessStatus));
        assert_eq!(err.err_args["status"], 500);
        assert_eq!(err.err_args["method"], "GET");
        assert_eq!(err.err_args["url"], format!("{base}/fail"));
        assert_eq!(err.err_args["body"], r#"{"error":"boom"}"#);
    }

    /// 2xx 但结构对不上：报 JsonError
    #[tokio::test]
    async fn send_decode_failure_is_json_error() {
        let base = serve_routes(ROUTES).await;
        let err = RequestConfig::plain(Method::GET, format!("{base}/bad-shape"))
            .send::<Json<Login>>(&local_config().build().unwrap())
            .await
            .unwrap_err();
        assert!(err.is(BaseErr::JsonError));
    }
}

/// 传输层失败：都报 BaseHttpErr::RequestFailed，原始 reqwest 错误挂在 source 上
mod transport_errors {
    use super::*;

    fn reqwest_source(err: &hygiea_core::HyErr) -> &reqwest::Error {
        err.source().unwrap().downcast_ref().unwrap()
    }

    /// 连不上：is_connect，参数里带方法和地址
    #[tokio::test]
    async fn connect_failure() {
        let base = closed_port_url().await;
        let err = RequestConfig::plain(Method::GET, format!("{base}/x"))
            .send::<Bytes>(&local_config().build().unwrap())
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::RequestFailed));
        assert_eq!(err.err_args["url"], format!("{base}/x"));
        assert!(reqwest_source(&err).is_connect());
    }

    /// BodyStream 读到一半断开：send 照样返回（响应头到了），读流时报 RequestFailed
    #[tokio::test]
    async fn stream_read_interrupted() {
        let base = serve(|_| Reply::json(200, "{\"a\":").truncated(50)).await;
        let mut r = RequestConfig::plain(Method::GET, format!("{base}/cut"))
            .send::<BodyStream>(&local_config().build().unwrap())
            .await
            .unwrap();
        let mut failed = None;
        while let Some(chunk) = r.body.next().await {
            if let Err(e) = chunk {
                failed = Some(e);
                break;
            }
        }
        assert!(failed.unwrap().is(BaseHttpErr::RequestFailed));
    }

    /// 响应头到了、body 读到一半断开：同样是 BaseHttpErr::RequestFailed，拿不到半截响应
    #[tokio::test]
    async fn body_read_interrupted() {
        let base = serve(|_| Reply::json(200, "{\"a\":").truncated(50)).await;
        let err = RequestConfig::plain(Method::GET, format!("{base}/cut"))
            .send::<Bytes>(&local_config().build().unwrap())
            .await
            .unwrap_err();
        assert!(err.is(BaseHttpErr::RequestFailed));
        assert!(reqwest_source(&err).is_body() || reqwest_source(&err).is_decode());
    }
}
