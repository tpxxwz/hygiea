//! socket 层的测试工具：模拟 HTTP mock 管不到的连接阶段。
//!
//! HTTP mock 是 TCP 握手完成之后才介入的：端口在监听，客户端发来的 SYN 由内核直接应答，所以它只能模拟
//! "连上之后慢"（`read_timeout` / `timeout`），模拟不了"连接阶段卡住"（`connect_timeout`）。
//!
//! 让 TCP 握手本身完不成，在本机做不到跨平台：把监听队列塞满的话，Linux 默认丢弃新 SYN（客户端会卡住），
//! macOS 却直接回 RST（客户端立刻失败）；丢包的防火墙规则又要 root。这里换成卡在 TLS 握手：
//! HTTP 客户端的 `connect_timeout` 一般覆盖 DNS、TCP 握手、TLS 握手三段，TCP 正常连上、TLS 等不到
//! 服务端的回应，建连阶段同样卡住直到超时。

use std::net::{SocketAddr, TcpListener};
use std::thread;

/// 本机上一个"建连卡住"的地址：接受 TCP 连接，但从不读写。用 `https://` 去连时 TLS 握手等不到
/// ServerHello，HTTP 客户端卡在建连阶段直到 `connect_timeout` 触发，用来测建连超时，不依赖外网和平台。
///
/// 只对先等服务端开口的协议有效：`http://` 明文请求会正常发出去，卡在等响应上，触发的是读超时而不是建连超时。
/// 接受下来的连接一直保持到进程结束（后台线程持有），drop 只是不再暴露地址，测试进程退出时一起释放
pub struct Silent {
    addr: SocketAddr,
}

impl Silent {
    /// 监听的地址，比如 `127.0.0.1:54321`
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `https://127.0.0.1:54321`，直接交给 HTTP 客户端
    pub fn https_url(&self) -> String {
        format!("https://{}", self.addr)
    }
}

/// 起一个只接受连接、从不读写的本地 TCP 服务，见 [`Silent`]。端口绑不上时 panic
pub fn silent() -> Silent {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("bind: {e}"));
    let addr = listener
        .local_addr()
        .unwrap_or_else(|e| panic!("local addr: {e}"));
    // 接受下来的连接要留着：drop 掉会关闭连接，客户端就不是卡住而是连接被关闭了
    thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    Silent { addr }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpStream;
    use std::time::Duration;

    use super::*;

    /// TCP 能连上，但服务端一个字节都不发：读到超时也没有数据
    #[test]
    fn accepts_but_never_speaks() {
        let target = silent();
        let mut stream = TcpStream::connect(target.addr()).expect("tcp connect should succeed");
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set read timeout");
        let mut buf = [0u8; 1];
        let err = stream
            .read(&mut buf)
            .expect_err("server must not send anything");
        assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "{err:?}"
        );
    }

    /// 用 https:// 去连：TLS 握手卡住，触发的是建连超时
    #[tokio::test]
    async fn https_connect_times_out() {
        let target = silent();
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_millis(200))
            .build()
            .expect("build client");
        let err = client
            .get(target.https_url())
            .send()
            .await
            .expect_err("tls handshake must not complete");
        assert!(err.is_connect() && err.is_timeout(), "{err:?}");
    }
}
