//! client 级配置：[`ClientConfig`] 及其到 `ClientBuilder` 的转换

use std::net::SocketAddr;
use std::time::Duration;

use reqwest::redirect::Policy;

use crate::HyErr;

use super::error::client_build_failed;
use super::parts::checked_headers;
use super::{Client, ClientBuilder, HeaderMap, IntoHeaders, Proxy};

/// `ClientBuilder` 的纯数据镜像：一个字段对一个 `ClientBuilder` 方法，字段顺序与方法声明顺序一致。
///
/// 用 [`ClientConfig::build`] 造出的 `Client` 内部是 `Arc`，连接池挂在它身上，
/// 所以要长期持有并共享（clone 很廉价）；在请求路径上反复构造等于池子永远是空的，每次都重新建连和握手。
///
/// 本 crate 的 `http` feature 已经把 reqwest 的 feature 集固定成
/// `json / query / form / stream / multipart / charset / gzip / brotli / zstd / deflate /
/// cookies / http2 / socks / system-proxy / rustls`，且 cargo feature 只增不减，下游关不掉，
/// 所以下面的字段一律可用，不需要额外开什么。
///
/// # 设计取舍
///
/// ## 压缩：四个开关合成一个
///
/// reqwest 的 `gzip` / `brotli` / `zstd` / `deflate` 合成一个
/// [`ClientConfig::transparent_compression`]，按算法单独开关没有真实场景，
/// 而且以后新增算法只需在 `From<ClientConfig> for ClientBuilder` 里多接一行，对外 API 不变。
///
/// ## 不收 `retry`
///
/// 它只覆盖协议层 nack（h2 的 GOAWAY(NO_ERROR) / RST_STREAM(REFUSED_STREAM)，即服务端明确
/// 表示没处理过请求的那一类），超时、连接中断、429、5xx 一概不管；而这些才是真正需要重试的场景，
/// 且要连带退避、抖动、幂等判断一起决定，交给调用方自己做更合适。只给一个覆盖一小块的旋钮，
/// 反而让人误以为重试已经配好了。另外 `retry::Builder` 是含闭包的 scoped 策略，本来也做不成纯数据。
///
/// 注意：**不配 `retry` 不等于关掉重试**。reqwest 在没有显式策略时跑的就是上面那套默认 nack 重试，
/// 这层仍然生效。真要覆盖它就 `ClientBuilder::from(config).retry(...)`。
///
/// ## 默认值够用而不收
///
/// `tcp_nodelay`：reqwest 默认已经是 `true`（即关掉 Nagle 算法）。HTTP 是请求/响应型协议，
/// 写完一个请求就等响应，没有小包可攒，开 Nagle 只会撞上对端 delayed ACK 白等几十毫秒。
///
/// `tcp_keepalive` / `tcp_keepalive_interval` / `tcp_keepalive_retries`：reqwest 默认
/// 15s / 15s / 3，也就是空闲 15 秒开始探、最坏 60 秒发现死连接。它的价值是把死连接提前踢出
/// 连接池，不需要按场景调参。
///
/// `pool_idle_timeout` / `pool_max_idle_per_host`：reqwest 默认 90s 回收、每 host 空闲连接数
/// 不设上限。后者只影响复用不限制并发，配合前者的回收足够了。
///
/// `http1_only` / `http2_prior_knowledge`：reqwest 默认走 ALPN 协商，https 下服务端支持就直接
/// 谈成 h2，不需要干预。`http2_prior_knowledge` 只对明文 h2c 有用，公网上基本不存在。
///
/// `connection_verbose`：对连接上的每次读写打一条 TRACE 日志，字节级的量，只在实验室里
/// 排查连接层问题时开几秒钟，不是常规配置项。
///
/// ## 不收 TLS 相关配置
///
/// 证书（`root_certificates` / `tls_certs_only` / `add_crl` / `identity`）、
/// 校验开关（`danger_accept_invalid_certs` / `danger_accept_invalid_hostnames` / `tls_sni`）、
/// 版本范围、`tls_sslkeylogfile`、`tls_info`、`https_only` 都没收。
/// reqwest + rustls 走的是 `rustls_platform_verifier`，也就是**操作系统的信任库**——
/// 公司私有 CA、自签 CA 只要装进容器的信任库（`/usr/local/share/ca-certificates/` +
/// `update-ca-certificates`，或 k8s 里挂 ConfigMap 到 `/etc/ssl/certs`），代码里什么都不用配。
/// 真需要 mTLS 客户端证书或临时放宽校验时，用 `ClientBuilder::from(config)` 拿到原生 builder 再接着链。
///
/// ## 不收 `local_address` / `interface`
///
/// 这两个是绑定出站源 IP 和出站网卡的，只对多路径的裸机/虚拟机有意义。容器和 k8s Pod 有独立的
/// network namespace，里面只有一个 Pod IP，节点上的 ENI 地址在 Pod 里根本不存在，绑了直接
/// `EADDRNOTAVAIL`；出口选择由 CNI、egress gateway 这些编排层决定。
/// 要切换出站 IP 用 [`ClientConfig::proxies`]。
///
/// ## 不收的 HTTP 版本相关配置
///
/// HTTP/1 的兼容开关：`http1_title_case_headers`、
/// `http1_allow_obsolete_multiline_headers_in_responses`、
/// `http1_ignore_invalid_headers_in_responses`、
/// `http1_allow_spaces_after_header_name_in_responses`、`http1_max_headers`、
/// `http09_responses`。都是给不守 RFC 的老服务端擦屁股用的，正常对端碰不到。
///
/// HTTP/2 的调优项：`http2_initial_stream_window_size`、
/// `http2_initial_connection_window_size`、`http2_adaptive_window`、`http2_max_frame_size`、
/// `http2_max_header_list_size`，以及 PING 探活的 `http2_keep_alive_interval`、
/// `http2_keep_alive_timeout`、`http2_keep_alive_while_idle`。窗口那几个只在「大文件 + 高延迟」
/// 同时成立时才有意义，PING 探活的职责又和默认就开着的 TCP keepalive 重叠。
///
/// HTTP/3 整组：`http3_prior_knowledge`、`http3_max_idle_timeout`、
/// `http3_stream_receive_window`、`http3_conn_receive_window`、`http3_send_window`、
/// `http3_congestion_bbr`、`http3_max_field_section_size`、`http3_send_grease`，
/// 以及只在 h3 下可用的 `tls_early_data`（TLS 1.3 的 0-RTT）。
/// 这组是被迫的：reqwest 把 http3 标为 unstable，光开 feature 不够，还必须设
/// `RUSTFLAGS='--cfg reqwest_unstable'`，否则 reqwest 自身 `compile_error!` 直接编译失败，
/// 而 RUSTFLAGS 是库没法替下游设的。等它稳定了再补。
///
/// ## 平台或类型限制而收不了的
///
/// - `tcp_user_timeout`：带 `#[cfg(any(target_os = "android", "fuchsia", "linux"))]`，
///   非 Linux 系平台上这个方法根本不存在。
/// - `unix_socket` / `windows_named_pipe`：让整个 `Client` 改走本机 IPC 而不是 TCP，
///   用于对话 Docker daemon 这类本地守护进程，和调远程 REST 接口无关。
/// - 以下是 trait 对象或含闭包，塞不进纯数据结构，需要时用 `ClientBuilder::from(config)` 拿到原生 builder
///   再接着链：`cookie_provider`、`redirect` 的自定义 `Policy`、`dns_resolver`、
///   `connector_layer`、`tls_backend_*`（TLS 后端由 feature 选定）。
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// 默认 User-Agent 头，每个请求都带；单请求显式设 `User-Agent` 会覆盖它
    pub user_agent: Option<String>,
    /// 每个请求都带的默认头，单请求同名头覆盖这里的值。
    /// 通过 [`ClientConfig::default_headers`] 添加会校验保留字段，直接改这个字段则不校验
    pub default_headers: HeaderMap,

    /// 进程内 cookie 罐子：自动存响应的 Set-Cookie，并在后续同域请求上带回去。
    pub cookie_store: bool,

    /// 透明压缩，一个开关管两端：请求上自动补 `Accept-Encoding` 宣告能收哪些编码，
    /// 响应按 `Content-Encoding` 自动解码。编译进来的算法（gzip / brotli / zstd / deflate）
    /// 一起开关，reqwest 以后新增的也自动跟上。请求 body 不会被压缩，这个方向 reqwest 不做。
    ///
    /// 只有请求头里既没有 `Accept-Encoding` 也没有 `Range` 时才会自动补；自己设了 `Accept-Encoding`
    /// 就等于接管这一端。发生解码时 `Content-Encoding` 和 `Content-Length` 会被一并删掉——
    /// 前者已经名不副实，后者描述的是解码前的线上字节数，而解码后的长度要读完才知道，
    /// 所以宁可返回 `None` 也不给一个错的数
    pub transparent_compression: bool,

    /// 重定向跟随上限，`None` = 不跟随、3xx 原样返回，超出上限报错
    pub max_redirects: Option<usize>,
    /// 跟随重定向时把上一跳的 URL 写进 `Referer` 头，降级到 http 时不写
    pub referer: bool,

    /// 显式代理，按加入顺序对每个请求依次匹配，第一个命中的生效。
    /// socks5:// 之类的 SOCKS 代理同样走这里。
    /// 不配时会自动拾取系统/环境变量里的代理设置
    pub proxies: Vec<Proxy>,
    /// 彻底禁用代理，连系统/环境变量里的也不认。和 `proxies` 同时给时以本项为准
    pub no_proxy: bool,

    /// 整个请求的时限：从发出到响应 body 读完。需要更长（比如大文件下载）时，
    /// 在 [`RequestConfig::timeout`](super::RequestConfig::timeout) 上按次覆盖即可（调大调小都行），
    /// 不必单开 `Client`；不能调成无限
    pub timeout: Option<Duration>,
    /// 对端最长多久不给数据。覆盖等响应头（服务端处理时间）和 body 分块之间的空闲两段，
    /// 每读到数据就重置，不累计总耗时，所以不会掐断持续传输的长下载。不能被单请求覆盖
    pub read_timeout: Option<Duration>,
    /// 建连时限，覆盖 DNS 解析、TCP 握手、TLS 握手三段，不含发请求和等响应
    pub connect_timeout: Option<Duration>,

    /// 写死的域名解析结果，绕过系统 DNS，相当于进程内 hosts。同一域名可给多个地址按顺序尝试。
    /// 端口只是占位，实际用 URL 里的端口
    pub resolve: Vec<(String, Vec<SocketAddr>)>,
}

impl ClientConfig {
    /// 等价于 [`ClientConfig::default`]
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置默认头，一次传全部，见 [`IntoHeaders`]。转换失败或使用了保留字段时返回 `Err`；
    /// 重复调用以最后一次为准
    pub fn default_headers(mut self, headers: impl IntoHeaders) -> Result<Self, HyErr> {
        self.default_headers = checked_headers(headers)?;
        Ok(self)
    }

    /// 追加一个代理，见 [`ClientConfig::proxies`]
    pub fn proxy(mut self, proxy: Proxy) -> Self {
        self.proxies.push(proxy);
        self
    }

    /// 追加一条写死的域名解析，见 [`ClientConfig::resolve`](#structfield.resolve)
    pub fn resolve(mut self, domain: impl Into<String>, addrs: Vec<SocketAddr>) -> Self {
        self.resolve.push((domain.into(), addrs));
        self
    }

    /// 造出 `Client`。reqwest 构建失败（比如 TLS 后端初始化失败）时返回
    /// [`ClientBuildFailed`](super::BaseHttpErr::ClientBuildFailed)，原始错误在 source 上
    pub fn build(self) -> Result<Client, HyErr> {
        ClientBuilder::from(self)
            .build()
            .map_err(client_build_failed)
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            user_agent: None,
            default_headers: HeaderMap::new(),
            cookie_store: false,
            transparent_compression: true,
            max_redirects: Some(10),
            referer: true,
            proxies: Vec::new(),
            no_proxy: false,
            timeout: Some(Duration::from_secs(5)),
            read_timeout: Some(Duration::from_secs(5)),
            connect_timeout: Some(Duration::from_secs(2)),
            resolve: Vec::new(),
        }
    }
}

impl From<ClientConfig> for ClientBuilder {
    fn from(config: ClientConfig) -> Self {
        let decompress = config.transparent_compression;
        let mut builder = Client::builder()
            .cookie_store(config.cookie_store)
            .gzip(decompress)
            .brotli(decompress)
            .zstd(decompress)
            .deflate(decompress)
            .redirect(match config.max_redirects {
                Some(max) => Policy::limited(max),
                None => Policy::none(),
            })
            .referer(config.referer);

        if let Some(ua) = config.user_agent {
            builder = builder.user_agent(ua);
        }
        if !config.default_headers.is_empty() {
            builder = builder.default_headers(config.default_headers);
        }
        for proxy in config.proxies {
            builder = builder.proxy(proxy);
        }
        if config.no_proxy {
            builder = builder.no_proxy();
        }
        if let Some(t) = config.timeout {
            builder = builder.timeout(t);
        }
        if let Some(t) = config.read_timeout {
            builder = builder.read_timeout(t);
        }
        if let Some(t) = config.connect_timeout {
            builder = builder.connect_timeout(t);
        }
        for (domain, addrs) in config.resolve {
            builder = builder.resolve_to_addrs(&domain, &addrs);
        }
        builder
    }
}

#[cfg(test)]
mod tests {
    use hygiea_test_support::headers::header_map;

    use crate::net::http::BaseHttpErr;

    use super::*;

    /// 默认值：和字段文档里写的一致
    mod defaults {
        use super::*;

        /// 逐个字段核对默认值，改默认值时这里会提醒同步文档
        #[test]
        fn match_documented_values() {
            let c = ClientConfig::default();
            assert_eq!(c.user_agent, None);
            assert!(c.default_headers.is_empty());
            assert!(!c.cookie_store);
            assert!(c.transparent_compression);
            assert_eq!(c.max_redirects, Some(10));
            assert!(c.referer);
            assert!(c.proxies.is_empty());
            assert!(!c.no_proxy);
            assert_eq!(c.timeout, Some(Duration::from_secs(5)));
            assert_eq!(c.read_timeout, Some(Duration::from_secs(5)));
            assert_eq!(c.connect_timeout, Some(Duration::from_secs(2)));
            assert!(c.resolve.is_empty());
        }

        /// `new()` 就是 `default()`
        #[test]
        fn new_equals_default() {
            assert_eq!(
                format!("{:?}", ClientConfig::new()),
                format!("{:?}", ClientConfig::default())
            );
        }
    }

    /// `default_headers()`：转换并拦截保留字段
    mod default_headers {
        use super::*;

        /// 普通头原样收下
        #[test]
        fn accepts_normal_headers() {
            let c = ClientConfig::new()
                .default_headers(header_map(&[("x-api-key", "k"), ("accept", "*/*")]))
                .unwrap();
            assert_eq!(c.default_headers["x-api-key"], "k");
            assert_eq!(c.default_headers["accept"], "*/*");
        }

        /// 保留字段一律拒绝，大小写不影响判断
        #[test]
        fn rejects_reserved_headers() {
            for name in [
                "Authorization",
                "content-type",
                "Content-Length",
                "transfer-encoding",
                "CONNECTION",
            ] {
                let err = ClientConfig::new()
                    .default_headers(header_map(&[(name, "x")]))
                    .unwrap_err();
                assert!(err.is(BaseHttpErr::InvalidHeader), "{name}");
            }
        }

        /// 重复调用以最后一次为准，不是合并
        #[test]
        fn later_call_replaces_earlier() {
            let c = ClientConfig::new()
                .default_headers(header_map(&[("x-a", "1")]))
                .unwrap()
                .default_headers(header_map(&[("x-b", "2")]))
                .unwrap();
            assert!(!c.default_headers.contains_key("x-a"));
            assert_eq!(c.default_headers["x-b"], "2");
        }
    }

    /// `proxy()` / `resolve()`：往列表里追加
    mod list_builders {
        use super::*;

        /// 代理按加入顺序保存，匹配时也按这个顺序
        #[test]
        fn proxy_appends_in_order() {
            let c = ClientConfig::new()
                .proxy(Proxy::http("http://127.0.0.1:1").unwrap())
                .proxy(Proxy::https("http://127.0.0.1:2").unwrap());
            assert_eq!(c.proxies.len(), 2);
            let debug = format!("{:?}", c.proxies);
            let (first, second) = (debug.find("port: Some(1)"), debug.find("port: Some(2)"));
            assert!(first.is_some() && first < second, "{debug}");
        }

        /// 解析规则按调用顺序追加，同一域名可以有多个地址
        #[test]
        fn resolve_appends() {
            let a: SocketAddr = "127.0.0.1:80".parse().unwrap();
            let b: SocketAddr = "127.0.0.2:80".parse().unwrap();
            let c = ClientConfig::new()
                .resolve("a.test", vec![a, b])
                .resolve(String::from("b.test"), vec![a]);
            assert_eq!(
                c.resolve,
                vec![
                    ("a.test".to_string(), vec![a, b]),
                    ("b.test".to_string(), vec![a])
                ]
            );
        }
    }

    /// `build()` / `From<ClientConfig> for ClientBuilder`：各个字段都能铺到 builder 上。
    /// 行为层面（UA 有没有发出去、重定向跟不跟）在集成测试 tests/http/client_config.rs 里验证
    mod build {
        use super::*;

        /// 默认配置能造出 Client
        #[test]
        fn default_config_builds() {
            ClientConfig::default().build().unwrap();
        }

        /// 每个字段都改成非默认值，也能造出 Client
        #[test]
        fn fully_customized_config_builds() {
            let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
            ClientConfig {
                user_agent: Some("hygiea-test".into()),
                default_headers: header_map(&[("x-a", "1")]),
                cookie_store: true,
                transparent_compression: false,
                max_redirects: None,
                referer: false,
                proxies: vec![Proxy::all("http://127.0.0.1:1").unwrap()],
                no_proxy: true,
                timeout: None,
                read_timeout: None,
                connect_timeout: None,
                resolve: vec![("a.test".into(), vec![addr])],
            }
            .build()
            .unwrap();
        }

        /// 拿到原生 builder 后还能接着链 reqwest 的其他方法
        #[test]
        fn converts_into_native_builder() {
            ClientBuilder::from(ClientConfig::new())
                .https_only(false)
                .build()
                .unwrap();
        }
    }
}
