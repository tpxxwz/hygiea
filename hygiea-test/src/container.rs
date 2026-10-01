//! 用代码启动容器，给 container 层的测试用（测试分层见 [crate 文档](crate)）。
//!
//! 这里只提供通用的启动能力，不写具体服务：镜像、环境变量、端口、怎样算就绪都由调用方传。
//!
//! ```ignore
//! let server = ContainerSpec::new("rustfs/rustfs", "1.0.0")
//!     .env("RUSTFS_ACCESS_KEY", "ak")
//!     .port(9000)
//!     .wait_http(9000, "/health", 200)
//!     .start()
//!     .await;
//! let endpoint = format!("http://{}", server.addr(9000));
//! ```
//!
//! 底层是 testcontainers，走 Docker Engine API：默认连本机 Docker；用 Podman 时把 `DOCKER_HOST`
//! 指向 Podman 的兼容 socket。[`RunningContainer`] drop 时删除容器，测试失败也不会留下容器。

use std::collections::HashMap;
use std::time::Duration;

use testcontainers::core::wait::{HttpWaitStrategy, LogWaitStrategy};
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// testcontainers 整个再导出，这里的接口不够用时直接用它
pub use testcontainers;

/// 要启动的容器：镜像加上各项设置，[`start`](ContainerSpec::start) 启动
#[derive(Debug, Clone)]
pub struct ContainerSpec {
    image: String,
    tag: String,
    env: Vec<(String, String)>,
    ports: Vec<u16>,
    cmd: Vec<String>,
    waits: Vec<Wait>,
    startup_timeout: Duration,
}

/// 怎样算启动完成，可以设多个，按顺序等
#[derive(Debug, Clone)]
enum Wait {
    /// 容器端口上的 HTTP 路径返回指定状态码
    Http {
        port: u16,
        path: String,
        status: u16,
    },
    /// stdout 或 stderr 里这段文字出现 `times` 次
    Log { message: String, times: usize },
}

impl ContainerSpec {
    /// 镜像名和 tag。tag 写固定版本，别用 latest，不然镜像更新会让测试结果不可复现
    pub fn new(image: impl Into<String>, tag: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            tag: tag.into(),
            env: Vec::new(),
            ports: Vec::new(),
            cmd: Vec::new(),
            waits: Vec::new(),
            startup_timeout: Duration::from_secs(60),
        }
    }

    /// 容器的环境变量
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// 暴露容器的 TCP 端口，映射到宿主机的随机端口，用 [`RunningContainer::port`] 查
    pub fn port(mut self, container_port: u16) -> Self {
        self.ports.push(container_port);
        self
    }

    /// 覆盖镜像默认的启动参数
    pub fn cmd(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.cmd = args.into_iter().map(Into::into).collect();
        self
    }

    /// 等容器端口上的 HTTP 路径返回 `status`。端口要先用 [`port`](ContainerSpec::port) 暴露
    pub fn wait_http(mut self, container_port: u16, path: impl Into<String>, status: u16) -> Self {
        self.waits.push(Wait::Http {
            port: container_port,
            path: path.into(),
            status,
        });
        self
    }

    /// 等 stdout 或 stderr 里出现 `message`
    pub fn wait_log(self, message: impl Into<String>) -> Self {
        self.wait_log_times(message, 1)
    }

    /// 等 stdout 或 stderr 里 `message` 出现 `times` 次。有的镜像启动时会先起一个临时实例做初始化再重启，
    /// 同一句就绪日志会打两次（比如 postgres 的 `database system is ready to accept connections`），
    /// 只等第一次会连到正在关闭的临时实例上
    pub fn wait_log_times(mut self, message: impl Into<String>, times: usize) -> Self {
        self.waits.push(Wait::Log {
            message: message.into(),
            times,
        });
        self
    }

    /// 启动加上等待就绪的总时限，默认 60 秒。首次拉镜像也算在里面
    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// 启动容器，等所有就绪条件满足后返回。起不来（没有容器运行时、拉不到镜像、超时）直接 panic
    pub async fn start(self) -> RunningContainer {
        let name = format!("{}:{}", self.image, self.tag);
        let mut image = GenericImage::new(self.image, self.tag);
        for port in &self.ports {
            image = image.with_exposed_port(ContainerPort::Tcp(*port));
        }
        for wait in self.waits {
            image = image.with_wait_for(match wait {
                Wait::Http { port, path, status } => WaitFor::http(
                    HttpWaitStrategy::new(path)
                        .with_port(ContainerPort::Tcp(port))
                        .with_expected_status_code(status),
                ),
                Wait::Log { message, times } => {
                    WaitFor::log(LogWaitStrategy::stdout_or_stderr(message).with_times(times))
                }
            });
        }
        let mut request = image.with_startup_timeout(self.startup_timeout);
        for (key, value) in self.env {
            request = request.with_env_var(key, value);
        }
        if !self.cmd.is_empty() {
            request = request.with_cmd(self.cmd);
        }

        let container = request.start().await.unwrap_or_else(|e| {
            panic!("start container {name}: {e} (is Docker running? for Podman set DOCKER_HOST)")
        });
        let host = container
            .get_host()
            .await
            .unwrap_or_else(|e| panic!("container {name} host: {e}"))
            .to_string();
        let mut ports = HashMap::new();
        for port in self.ports {
            let mapped = container
                .get_host_port_ipv4(ContainerPort::Tcp(port))
                .await
                .unwrap_or_else(|e| panic!("container {name} port {port}: {e}"));
            ports.insert(port, mapped);
        }
        RunningContainer {
            name,
            host,
            ports,
            _container: container,
        }
    }
}

/// 运行中的容器。drop 时删除容器
pub struct RunningContainer {
    name: String,
    host: String,
    ports: HashMap<u16, u16>,
    _container: ContainerAsync<GenericImage>,
}

impl RunningContainer {
    /// 从宿主机访问容器用的主机名，一般是 `localhost`
    pub fn host(&self) -> &str {
        &self.host
    }

    /// 容器端口映射到的宿主机端口。端口没有用 [`ContainerSpec::port`] 暴露时 panic
    pub fn port(&self, container_port: u16) -> u16 {
        match self.ports.get(&container_port) {
            Some(port) => *port,
            None => panic!(
                "container {} port {container_port} not exposed; call ContainerSpec::port first",
                self.name
            ),
        }
    }

    /// `host:port`，拼 URL 用，比如 `format!("http://{}", c.addr(9000))`
    pub fn addr(&self, container_port: u16) -> String {
        format!("{}:{}", self.host, self.port(container_port))
    }
}

#[cfg(test)]
#[crate::container]
mod tests {
    use super::*;

    /// 环境变量和 cmd 生效，日志里出现指定文字才算就绪
    fn echo_ready() -> ContainerSpec {
        ContainerSpec::new("alpine", "3.20")
            .env("GREETING", "hello")
            .cmd(["sh", "-c", "echo ready-$GREETING; sleep 30"])
            .wait_log("ready-hello")
    }

    #[tokio::test]
    async fn env_cmd_and_log_wait() {
        let c = echo_ready().start().await;
        assert!(!c.host().is_empty());
    }

    /// 查没暴露的端口时 panic，提示先调用 ContainerSpec::port
    #[tokio::test]
    #[should_panic(expected = "port 8080 not exposed")]
    async fn unexposed_port_panics() {
        let c = echo_ready().start().await;
        c.port(8080);
    }
}
