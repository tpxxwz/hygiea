//! 本地 HTTP 服务：给 http 客户端的端到端测试用。
//!
//! 每个连接只处理一个请求，响应都带 `connection: close`。能回显请求（[`echo`]）、按路径回固定响应
//! （[`serve_routes`]），也能构造延迟、截断 body 这类异常响应（[`Reply`]）

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 服务端收到的请求。不叫 Request 是为了不和 reqwest 的同名类型撞上（测试里常 glob import http 模块）
pub struct Received {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    /// 名字统一小写
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// 服务端要回的响应
pub struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// 收到请求之后、开始回任何东西之前等多久，用来测连接建立后的整体超时
    delay: Duration,
    /// 头发完之后、写 body 之前再等多久，用来测 read_timeout（头和 body 之间的空闲）
    body_delay: Duration,
    /// 声明的 content-length 比实际 body 多出的字节数，用来模拟读 body 中途断开
    missing_bytes: usize,
    /// 收到请求后直接重置连接（RST），不写任何响应
    reset: bool,
}

impl Reply {
    /// JSON 响应
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into().into_bytes(),
            delay: Duration::ZERO,
            body_delay: Duration::ZERO,
            missing_bytes: 0,
            reset: false,
        }
    }

    /// 没有 body 的响应
    pub fn empty(status: u16) -> Self {
        Self {
            headers: Vec::new(),
            ..Self::json(status, "")
        }
    }

    /// 任意字节的响应，构造非 UTF-8 的 body（比如别的字符集编码出来的文本）
    pub fn bytes(status: u16, content_type: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            headers: vec![("content-type".into(), content_type.into())],
            body,
            ..Self::empty(status)
        }
    }

    /// 收到请求后直接重置连接（RST），不写任何响应，用来模拟对端连接被重置
    pub fn reset() -> Self {
        Self {
            reset: true,
            ..Self::empty(0)
        }
    }

    /// 加一个响应头
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// 先等一会儿再回，用来测连接建立后的整体超时
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// 头发完之后再等一会儿才写 body，用来测 read_timeout：头很快到，body 隔了很久才来
    pub fn body_delay(mut self, delay: Duration) -> Self {
        self.body_delay = delay;
        self
    }

    /// 声明的长度比实际多，写完就断开，客户端读 body 会失败
    pub fn truncated(mut self, missing_bytes: usize) -> Self {
        self.missing_bytes = missing_bytes;
        self
    }
}

/// 把收到的请求原样回成 JSON：`{method, path, query, headers, body}`
pub fn echo(req: &Received) -> Reply {
    let headers: serde_json::Map<String, serde_json::Value> = req
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone().into()))
        .collect();
    let body = serde_json::json!({
        "method": req.method,
        "path": req.path,
        "query": req.query,
        "headers": headers,
        "body": String::from_utf8_lossy(&req.body),
    });
    Reply::json(200, body.to_string())
}

/// 起一个本地服务，每个请求交给 `handler`，返回 `http://127.0.0.1:端口`
pub async fn serve<F>(handler: F) -> String
where
    F: Fn(&Received) -> Reply + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            tokio::spawn(async move { handle(socket, &*handler).await });
        }
    });
    format!("http://{addr}")
}

/// 按路径返回固定响应：`(路径, 状态码, JSON body)`，没匹配上的回 404
pub async fn serve_routes(routes: &'static [(&'static str, u16, &'static str)]) -> String {
    serve(|req| {
        routes
            .iter()
            .find(|(path, _, _)| *path == req.path)
            .map(|(_, status, body)| Reply::json(*status, *body))
            .unwrap_or_else(|| Reply::empty(404))
    })
    .await
}

/// 一个已经关掉的端口，连上去必定失败
pub async fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

async fn handle(mut socket: TcpStream, handler: &(dyn Fn(&Received) -> Reply + Send + Sync)) {
    let Some(req) = read_request(&mut socket).await else {
        return;
    };
    let resp = handler(&req);
    if resp.reset {
        // linger(0) 后丢弃连接触发 RST，而不是正常的四次挥手，用来模拟连接被重置。
        // tokio 标了 deprecated（说会在 drop 时阻塞线程），但这里就是故意要一个同步、
        // 立即生效的重置，本地回环延迟可忽略不计
        #[allow(deprecated)]
        let _ = socket.set_linger(Some(Duration::ZERO));
        return;
    }
    if !resp.delay.is_zero() {
        tokio::time::sleep(resp.delay).await;
    }
    // 原因短语可以为空（RFC 9112），客户端只看状态码。
    // 每个连接只处理一个请求，明确告诉客户端别复用，免得复用到已经关掉的连接
    let mut head = format!(
        "HTTP/1.1 {} \r\ncontent-length: {}\r\nconnection: close\r\n",
        resp.status,
        resp.body.len() + resp.missing_bytes
    );
    for (name, value) in &resp.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = socket.write_all(head.as_bytes()).await;
    if !resp.body_delay.is_zero() {
        tokio::time::sleep(resp.body_delay).await;
    }
    let _ = socket.write_all(&resp.body).await;
    let _ = socket.shutdown().await;
}

/// 读一个完整的 HTTP/1.1 请求：请求行、头、按 content-length 读 body
async fn read_request(socket: &mut TcpStream) -> Option<Received> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let target = request_line.next()?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (target.to_string(), None),
    };
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();

    // 流式上传不知道长度，客户端按 chunked 发，读到结束块 `0\r\n\r\n` 为止再解码
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.eq_ignore_ascii_case("chunked"));
    let body = if chunked {
        while !buf[head_end..].ends_with(b"0\r\n\r\n") {
            let n = socket.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        decode_chunked(&buf[head_end..])
    } else {
        let len: usize = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let n = socket.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        buf[head_end..].to_vec()
    };
    Some(Received {
        method,
        path,
        query,
        headers,
        body,
    })
}

/// 解 chunked 编码：每块是 `十六进制长度\r\n数据\r\n`，长度为 0 的块表示结束。块扩展（`;` 之后）忽略
fn decode_chunked(mut raw: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    while let Some(line_end) = raw.windows(2).position(|w| w == b"\r\n") {
        let size_line = String::from_utf8_lossy(&raw[..line_end]);
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let Ok(size) = usize::from_str_radix(size_hex, 16) else {
            break;
        };
        if size == 0 {
            break;
        }
        let start = line_end + 2;
        let Some(data) = raw.get(start..start + size) else {
            break;
        };
        body.extend_from_slice(data);
        raw = raw.get(start + size + 2..).unwrap_or_default();
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    mod decode_chunked_tests {
        use super::*;

        /// 多个块按顺序拼起来
        #[test]
        fn multiple_chunks() {
            let raw = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
            assert_eq!(decode_chunked(raw), b"hello world");
        }

        /// 块扩展（`;` 之后的部分）被忽略，不影响长度解析
        #[test]
        fn chunk_extension_is_ignored() {
            let raw = b"5;foo=bar\r\nhello\r\n0\r\n\r\n";
            assert_eq!(decode_chunked(raw), b"hello");
        }

        /// 长度字段本身不是合法的十六进制：直接停止，已解出的块不受影响
        #[test]
        fn invalid_size_hex_stops_decoding() {
            let raw = b"3\r\nfoo\r\nzz\r\ndata\r\n0\r\n\r\n";
            assert_eq!(decode_chunked(raw), b"foo");
        }

        /// 声明的长度比实际给的数据长：拿不到完整的这一块，直接停止
        #[test]
        fn declared_length_exceeds_available_data() {
            let raw = b"a\r\nhi\r\n"; // 声明 0xa=10 字节，实际只给了 2
            assert_eq!(decode_chunked(raw), b"");
        }

        /// 数据在块中途被截断（既没读满声明的长度，也没有结束块）：已经解出的前面几块保留
        #[test]
        fn truncated_chunk_keeps_earlier_chunks() {
            let raw = b"3\r\nfoo\r\n5\r\nab"; // 第二块声明 5 字节，实际只给了 2 字节就断了
            assert_eq!(decode_chunked(raw), b"foo");
        }
    }

    mod read_request_tests {
        use super::*;

        /// 起一对本地已连接的 socket：一端模拟客户端发请求，另一端交给 `read_request` 解析
        async fn connected_pair() -> (TcpStream, TcpStream) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let client = TcpStream::connect(addr).await.unwrap();
            let (server, _) = listener.accept().await.unwrap();
            (client, server)
        }

        /// 请求行里的 query 被拆出来，没有的话是 None
        #[tokio::test]
        async fn parses_query_string() {
            let (mut client, mut server) = connected_pair().await;
            client
                .write_all(b"GET /a?x=1&y=2 HTTP/1.1\r\nhost: h\r\n\r\n")
                .await
                .unwrap();
            let req = read_request(&mut server).await.unwrap();
            assert_eq!(req.path, "/a");
            assert_eq!(req.query.as_deref(), Some("x=1&y=2"));

            let (mut client, mut server) = connected_pair().await;
            client
                .write_all(b"GET /b HTTP/1.1\r\nhost: h\r\n\r\n")
                .await
                .unwrap();
            let req = read_request(&mut server).await.unwrap();
            assert_eq!(req.query, None);
        }

        /// chunked 请求体读到结束块为止，解码后就是原文
        #[tokio::test]
        async fn decodes_chunked_body() {
            let (mut client, mut server) = connected_pair().await;
            client
                .write_all(
                    b"POST /p HTTP/1.1\r\n\
                      transfer-encoding: chunked\r\n\r\n\
                      5\r\nhello\r\n0\r\n\r\n",
                )
                .await
                .unwrap();
            let req = read_request(&mut server).await.unwrap();
            assert_eq!(req.method, "POST");
            assert_eq!(req.body, b"hello");
        }
    }
}
