//! reqwest 的客户端配置 [`ReqwestConfig`] 和应用组件 [`ReqwestComponent`]。

use std::net::SocketAddr;
use std::time::Duration;

use reqwest::redirect::Policy;
use serde::Deserialize;

use hygiea_core::app::{AppErr, ConfigResource, ImmediateResourceComponent, async_trait};
use hygiea_core::{Result, err};

use super::HeaderMap;
use super::config::{HeaderMapConfig, ProxyConfig};
use super::error::{client_build_failed, invalid_header};
use super::headers::{IntoHeaders, checked_headers};
use reqwest::{Client, ClientBuilder};

// ---------------------------- 客户端配置 ----------------------------

/// `ClientBuilder` 的纯数据镜像：一个字段对一个 `ClientBuilder` 方法，字段顺序与方法声明顺序一致。
///
/// 用 [`ConfigResource::from_config`] 造出的 [`ReqwestClient`] 内部是 `Arc`，连接池挂在它身上，
/// 所以要长期持有并共享（clone 很廉价）；在请求路径上反复构造等于池子永远是空的，每次都重新建连和握手。
///
/// `hygiea-http-client` crate 已经把 reqwest 的 feature 集固定成
/// `json / query / form / stream / multipart / charset / gzip / brotli / zstd / deflate /
/// cookies / http2 / socks / system-proxy / rustls`，且 cargo feature 只增不减，下游关不掉，
/// 所以下面的字段一律可用，不需要额外开什么。
///
/// # 设计取舍
///
/// ## 压缩：四个开关合成一个
///
/// reqwest 的 `gzip` / `brotli` / `zstd` / `deflate` 合成一个
/// [`ReqwestConfig::transparent_compression`]，按算法单独开关没有真实场景，
/// 而且以后新增算法只需在 `client_builder` 里多接一行，对外 API 不变。
///
/// ## 不收 `retry`
///
/// 它只覆盖协议层 nack（h2 的 GOAWAY(NO_ERROR) / RST_STREAM(REFUSED_STREAM)，即服务端明确
/// 表示没处理过请求的那一类），超时、连接中断、429、5xx 一概不管；而这些才是真正需要重试的场景，
/// 且要连带退避、抖动、幂等判断一起决定，交给调用方自己做更合适。只给一个覆盖一小块的旋钮，
/// 反而让人误以为重试已经配好了。另外 `retry::Builder` 是含闭包的 scoped 策略，本来也做不成纯数据。
///
/// 注意：**不配 `retry` 不等于关掉重试**。reqwest 在没有显式策略时跑的就是上面那套默认 nack 重试，
/// 这层仍然生效，目前没有覆盖它的入口。
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
/// mTLS 客户端证书、临时放宽校验目前不支持，要用时再加进配置。
///
/// ## 不收 `local_address` / `interface`
///
/// 这两个是绑定出站源 IP 和出站网卡的，只对多路径的裸机/虚拟机有意义。容器和 k8s Pod 有独立的
/// network namespace，里面只有一个 Pod IP，节点上的 ENI 地址在 Pod 里根本不存在，绑了直接
/// `EADDRNOTAVAIL`；出口选择由 CNI、egress gateway 这些编排层决定。
/// 要切换出站 IP 用 [`ReqwestConfig::proxies`]。
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
/// - 以下是 trait 对象或含闭包，塞不进纯数据结构，目前不支持：`cookie_provider`、`redirect` 的自定义 `Policy`、
///   `dns_resolver`、`connector_layer`、`tls_backend_*`（TLS 后端由 feature 选定）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReqwestConfig {
    /// 默认 User-Agent 头，每个请求都带；单请求显式设 `User-Agent` 会覆盖它
    pub user_agent: Option<String>,
    /// 每个请求都带的默认头，单请求同名头覆盖这里的值。
    /// 配置文件支持字符串、同名头的值列表，以及带 `sensitive` 标记的值
    /// 通过 [`ReqwestConfig::default_headers`] 添加会校验保留字段，直接改这个字段则不校验
    pub default_headers: HeaderMapConfig,

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

    /// 重定向跟随上限，`0` = 不跟随、3xx 原样返回，超出上限报错
    pub max_redirects: usize,
    /// 跟随重定向时把上一跳的 URL 写进 `Referer` 头，降级到 http 时不写
    pub referer: bool,

    /// 显式代理，按加入顺序对每个请求依次匹配，第一个命中的生效。
    /// socks5:// 之类的 SOCKS 代理同样走这里。配置文件支持认证、代理头和逐代理排除列表。
    /// 不配时会自动拾取系统/环境变量里的代理设置
    pub proxies: Vec<ProxyConfig>,
    /// 彻底禁用代理，连系统/环境变量里的也不认。和 `proxies` 同时给时以本项为准
    pub no_proxy: bool,

    /// 整个请求的时限：从发出到响应 body 读完。需要更长（比如大文件下载）时，
    /// 在 [`RequestConfig::timeout`](super::RequestConfig::timeout) 上按次覆盖即可（调大调小都行），
    /// 不必单开 `Client`；不配置则不设客户端级总时限
    pub timeout: Option<Duration>,
    /// 对端最长多久不给数据。覆盖等响应头（服务端处理时间）和 body 分块之间的空闲两段，
    /// 每读到数据就重置，不累计总耗时，所以不会掐断持续传输的长下载。不能被单请求覆盖
    pub read_timeout: Option<Duration>,
    /// 建连时限，覆盖 DNS 解析、TCP 握手、TLS 握手三段，不含发请求和等响应
    pub connect_timeout: Option<Duration>,

    /// 写死的域名解析结果，绕过系统 DNS，相当于进程内 hosts。同一域名可给多个地址按顺序尝试。
    /// 端口只是占位，实际用 URL 里的端口
    pub resolve: Vec<(String, Vec<SocketAddr>)>,

    // ---- 以下是 reqwest 没有、本模块自己加的 ----
    /// 打开 debug 日志：每次请求打 start（请求）和结束（请求加响应）两条 JSON，url、params、body、
    /// 响应原文，带请求头和响应头，不打码，无视 [`RequestConfig::enable_logging`](super::RequestConfig::enable_logging)。
    /// 只在 `debug-log` feature 下存在，用于测试排查。这是初始值，build 之后用
    /// [`ReqwestClient::set_debug`] 改；JSON 默认一行，[`ReqwestClient::set_pretty`] 打开缩进
    #[cfg(feature = "debug-log")]
    pub debug: bool,
}

impl Default for ReqwestConfig {
    fn default() -> Self {
        Self {
            user_agent: None,
            default_headers: HeaderMapConfig::default(),
            cookie_store: false,
            transparent_compression: true,
            max_redirects: 10,
            referer: true,
            proxies: Vec::new(),
            no_proxy: false,
            timeout: None,
            read_timeout: None,
            connect_timeout: None,
            resolve: Vec::new(),
            #[cfg(feature = "debug-log")]
            debug: false,
        }
    }
}

impl ReqwestConfig {
    /// 添加默认头，见 [`IntoHeaders`]。多次调用、或者一次传数组 / 元组，都和已有的（包括配置文件里的）按顺序合并，
    /// 同名的以靠后的为准。转换失败、使用了保留字段、或者已有的默认头本身不合法时返回 `Err`
    pub fn default_headers(mut self, headers: impl IntoHeaders) -> Result<Self> {
        let mut merged = HeaderMap::try_from(std::mem::take(&mut self.default_headers))
            .map_err(invalid_header)?;
        merged.extend(checked_headers(headers)?);
        self.default_headers = merged.into();
        Ok(self)
    }

    /// 追加一个代理，见 [`ReqwestConfig::proxies`]
    pub fn proxy(mut self, proxy: ProxyConfig) -> Self {
        self.proxies.push(proxy);
        self
    }

    /// 追加一条写死的域名解析，见 [`ReqwestConfig::resolve`](#structfield.resolve)
    pub fn resolve(mut self, domain: impl Into<String>, addrs: Vec<SocketAddr>) -> Self {
        self.resolve.push((domain.into(), addrs));
        self
    }
}

// ---------------------------- 客户端 ----------------------------

/// 组件提供的 HTTP 客户端，用 [`ConfigResource::from_config`] 造出来，传给
/// [`RequestConfig::send`](super::RequestConfig::send) 发请求。内部是 `Arc`，clone 很便宜，要长期持有并共享
#[derive(Debug, Clone)]
pub struct ReqwestClient {
    pub(super) inner: Client,
    /// debug 日志的状态（client 级默认头、两个开关），所有 clone 共享，见 debug.rs
    #[cfg(feature = "debug-log")]
    pub(super) debug: std::sync::Arc<super::debug::DebugState>,
}

/// 把配置铺到 reqwest 原生的 `ClientBuilder` 上。不对外：原生的 `Client` 用不了本模块的 `send`
fn client_builder(config: ReqwestConfig) -> Result<ClientBuilder> {
    let mut builder = Client::builder();
    if config.cookie_store {
        builder = builder.cookie_store(true);
    }
    if !config.transparent_compression {
        builder = builder.gzip(false).brotli(false).zstd(false).deflate(false);
    }
    if config.max_redirects == 0 {
        builder = builder.redirect(Policy::none());
    } else if config.max_redirects != 10 {
        builder = builder.redirect(Policy::limited(config.max_redirects));
    }
    if !config.referer {
        builder = builder.referer(false);
    }

    if let Some(ua) = config.user_agent {
        builder = builder.user_agent(ua);
    }
    let headers = HeaderMap::try_from(config.default_headers)
        .map_err(|e| err!(AppErr::InvalidConfig, format!("default headers: {e}")))?;
    if !headers.is_empty() {
        builder = builder.default_headers(headers);
    }
    for proxy in config.proxies {
        builder = builder.proxy(proxy.try_into()?);
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
    Ok(builder)
}

// ---------------------------- 应用组件 ----------------------------

#[async_trait]
impl ConfigResource for ReqwestClient {
    type Config = ReqwestConfig;

    /// 请求头无效时返回 `InvalidConfig`；代理 URL 或 reqwest 构建失败时返回
    /// [`ClientBuildFailed`](super::HttpClientErr::ClientBuildFailed)，reqwest 错误保留在 source 上
    async fn from_config(config: &ReqwestConfig) -> Result<Self> {
        // reqwest 先校验（UA 不合法时报 ClientBuildFailed），再算 debug 状态
        let inner = client_builder(config.clone())?
            .build()
            .map_err(client_build_failed)?;
        Ok(ReqwestClient {
            inner,
            #[cfg(feature = "debug-log")]
            debug: super::debug::DebugState::new(
                config.debug,
                config.user_agent.clone(),
                config.default_headers.clone(),
            )?,
        })
    }
}

/// 构建 HTTP 客户端，并按组件名注册到 `Resources`。用法见 [`ImmediateResourceComponent`]
pub type ReqwestComponent = ImmediateResourceComponent<ReqwestClient>;

#[cfg(test)]
mod tests {
    use crate::reqwest_client::error::HttpClientErr;
    use std::error::Error as _;
    use test_support::headers::header_map;

    use super::*;

    /// 默认值：和字段文档里写的一致
    mod defaults {
        use super::*;

        /// 逐个字段核对默认值，改默认值时这里会提醒同步文档
        #[test]
        fn match_documented_values() {
            let c = ReqwestConfig::default();
            assert_eq!(c.user_agent, None);
            assert!(c.default_headers.is_empty());
            assert!(!c.cookie_store);
            assert!(c.transparent_compression);
            assert_eq!(c.max_redirects, 10);
            assert!(c.referer);
            assert!(c.proxies.is_empty());
            assert!(!c.no_proxy);
            assert_eq!(c.timeout, None);
            assert_eq!(c.read_timeout, None);
            assert_eq!(c.connect_timeout, None);
            assert!(c.resolve.is_empty());
            #[cfg(feature = "debug-log")]
            assert!(!c.debug);
        }
    }

    /// `default_headers()`：转换并拦截保留字段
    mod default_headers {
        use super::*;

        /// 普通头原样收下
        #[test]
        fn accepts_normal_headers() {
            let c = ReqwestConfig::default()
                .default_headers(header_map(&[("x-api-key", "k"), ("accept", "*/*")]))
                .unwrap();
            let headers = HeaderMap::try_from(c.default_headers).unwrap();
            assert_eq!(headers["x-api-key"], "k");
            assert_eq!(headers["accept"], "*/*");
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
                let err = ReqwestConfig::default()
                    .default_headers(header_map(&[(name, "x")]))
                    .unwrap_err();
                assert!(err.is(HttpClientErr::InvalidHeader), "{name}");
            }
        }

        /// 多次调用按顺序合并：不同名的都保留，同名的以后一次为准
        #[test]
        fn later_call_merges_into_earlier() {
            let c = ReqwestConfig::default()
                .default_headers(header_map(&[("x-a", "1"), ("x-b", "old")]))
                .unwrap()
                .default_headers(header_map(&[("x-b", "new"), ("x-c", "3")]))
                .unwrap();
            let headers = HeaderMap::try_from(c.default_headers).unwrap();
            assert_eq!(headers["x-a"], "1");
            assert_eq!(headers.get_all("x-b").iter().count(), 1);
            assert_eq!(headers["x-b"], "new");
            assert_eq!(headers["x-c"], "3");
        }
    }

    /// `proxy()` / `resolve()`：往列表里追加
    mod list_builders {
        use super::*;

        /// 代理按加入顺序保存，匹配时也按这个顺序
        #[test]
        fn proxy_appends_in_order() {
            let c = ReqwestConfig::default()
                .proxy(ProxyConfig::http("http://127.0.0.1:1"))
                .proxy(ProxyConfig::https("http://127.0.0.1:2"));
            assert_eq!(c.proxies.len(), 2);
            assert_eq!(c.proxies[0].url, "http://127.0.0.1:1");
            assert_eq!(c.proxies[1].url, "http://127.0.0.1:2");
        }

        /// 解析规则按调用顺序追加，同一域名可以有多个地址
        #[test]
        fn resolve_appends() {
            let a: SocketAddr = "127.0.0.1:80".parse().unwrap();
            let b: SocketAddr = "127.0.0.2:80".parse().unwrap();
            let c = ReqwestConfig::default()
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

    /// `build()` / `client_builder()`：各个字段都能铺到 builder 上。
    /// 行为层面（UA 有没有发出去、重定向跟不跟）在集成测试 tests/reqwest_client/client_config.rs 里验证
    mod build {
        use super::*;

        /// 默认配置能造出 Client
        #[tokio::test]
        async fn default_config_builds() {
            ReqwestClient::from_config(&ReqwestConfig::default())
                .await
                .unwrap();
        }

        /// 组合多项非默认配置也能造出 Client
        #[tokio::test]
        async fn configured_fields_build() {
            let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
            ReqwestClient::from_config(&ReqwestConfig {
                user_agent: Some("hygiea-test".into()),
                default_headers: header_map(&[("x-a", "1")]).into(),
                cookie_store: true,
                transparent_compression: false,
                max_redirects: 0,
                referer: false,
                proxies: vec![ProxyConfig::all("http://127.0.0.1:1")],
                no_proxy: true,
                timeout: None,
                read_timeout: None,
                connect_timeout: None,
                resolve: vec![("a.test".into(), vec![addr])],
                #[cfg(feature = "debug-log")]
                debug: true,
            })
            .await
            .unwrap();
        }

        /// reqwest 在构建 Client 时校验 User-Agent，错误保留为 source
        #[tokio::test]
        async fn invalid_user_agent_is_client_build_failed() {
            let err = ReqwestClient::from_config(&ReqwestConfig {
                user_agent: Some("bad\nvalue".into()),
                ..ReqwestConfig::default()
            })
            .await
            .unwrap_err();
            assert!(err.is(HttpClientErr::ClientBuildFailed));
            assert!(err.source().is_some());
        }
    }

    /// 从配置文件（TOML）反序列化 `ReqwestConfig`：字段格式、默认值、非法值在什么时候报错
    mod config_file {
        use super::*;

        /// 空配置等于代码默认值
        #[test]
        fn empty_toml_uses_defaults() {
            let parsed: ReqwestConfig = toml::from_str("").unwrap();
            assert_eq!(
                format!("{parsed:?}"),
                format!("{:?}", ReqwestConfig::default())
            );
        }

        /// 拼错的字段名直接报错，不会被悄悄忽略
        #[test]
        fn unknown_field_is_rejected() {
            assert!(toml::from_str::<ReqwestConfig>("max_redirect = 0").is_err());
            assert!(
                toml::from_str::<ReqwestConfig>("[[proxies]]\nurl = \"http://x\"\nuser = \"u\"")
                    .is_err()
            );
        }

        /// 超时、resolve、代理这些字段转成运行时类型
        #[tokio::test]
        async fn fields_convert_to_runtime_types() {
            let config: ReqwestConfig = toml::from_str(
                r#"
        timeout = { secs = 5, nanos = 0 }
        read_timeout = { secs = 7, nanos = 250000000 }
        connect_timeout = { secs = 2, nanos = 0 }
        max_redirects = 0
        resolve = [["example.com", ["127.0.0.1:80"]]]
        [default_headers]
        X-Api-Key = "key"
        [[proxies]]
        kind = "http"
        url = "http://127.0.0.1:7890"
        "#,
            )
            .unwrap();
            assert_eq!(config.timeout, Some(Duration::from_secs(5)));
            assert_eq!(config.read_timeout, Some(Duration::new(7, 250_000_000)));
            assert_eq!(config.connect_timeout, Some(Duration::from_secs(2)));
            assert_eq!(config.max_redirects, 0);
            let headers = HeaderMap::try_from(config.default_headers.clone()).unwrap();
            assert_eq!(headers["x-api-key"], "key");
            assert_eq!(config.proxies.len(), 1);
            assert_eq!(config.resolve[0].0, "example.com");
            assert_eq!(config.resolve[0].1[0].to_string(), "127.0.0.1:80");
            assert!(ReqwestClient::from_config(&config).await.is_ok());
        }

        /// 请求头的三种写法：字符串、同名头列表、带 sensitive 的文本或字节
        #[test]
        fn header_value_forms() {
            let config: ReqwestConfig = toml::from_str(
                r#"
        [default_headers]
        x-repeat = ["one", "two"]
        authorization = { value = "Bearer secret", sensitive = true }
        x-binary = { bytes = [128, 65], sensitive = true }
        "#,
            )
            .unwrap();
            let headers = HeaderMap::try_from(config.default_headers).unwrap();
            let values: Vec<_> = headers.get_all("x-repeat").iter().collect();
            assert_eq!(values, ["one", "two"]);
            assert!(headers["authorization"].is_sensitive());
            assert_eq!(headers["x-binary"].as_bytes(), &[128, 65]);
            assert!(headers["x-binary"].is_sensitive());
        }

        /// 配置文件不校验保留头（`ReqwestConfig::default_headers` 方法才校验），原样保留
        #[test]
        fn reserved_header_is_kept() {
            let config: ReqwestConfig =
                toml::from_str("[default_headers]\ncontent-type = \"application/json\"").unwrap();
            let headers = HeaderMap::try_from(config.default_headers).unwrap();
            assert_eq!(headers["content-type"], "application/json");
        }

        /// 反序列化只管格式，非法的头名、头值、代理 URL 到 build 时才报错
        mod rejected_when_building {
            use super::*;

            async fn build_err(toml: &str) {
                let config: ReqwestConfig = toml::from_str(toml).unwrap();
                assert!(ReqwestClient::from_config(&config).await.is_err(), "{toml}");
            }

            #[tokio::test]
            async fn invalid_header_name() {
                build_err("[default_headers]\n\"bad name\" = \"x\"").await;
            }

            #[tokio::test]
            async fn invalid_header_value() {
                build_err("[default_headers]\nx-key = \"bad\\nvalue\"").await;
            }

            #[tokio::test]
            async fn invalid_proxy_url() {
                build_err("[[proxies]]\nurl = \"not a url\"").await;
            }

            #[tokio::test]
            async fn invalid_proxy_header() {
                build_err(
                    "[[proxies]]\nurl = \"http://127.0.0.1:7890\"\nheaders = { \"bad name\" = \"x\" }",
                ).await;
            }
        }
    }
}
