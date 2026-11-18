//! 进程控制（feature `app-daemon`）：pid 文件、后台运行、停掉在跑的、查状态。Linux、macOS、Windows 都支持：
//! 后台起用 process-wrap，判断进程在不在用 sysinfo。停在各平台上都走正常关闭，见下面的「停」。
//!
//! - pid 文件：框架配置 `[registry] pid_file` 写了路径时，[`Registry::run`](super::Registry::run) 启动时写进自己的进程号，
//!   正常退出时删掉（只删自己写的）。启动时文件里的进程还在，报 [`AppErr::AlreadyRunning`]，同一个应用不会起两份；
//!   进程已经没了（上次被强杀）的旧文件直接覆盖。
//! - 后台运行：命令行合进 [`DaemonArgs`]（`-d` / `--detach`）。带了它时应用调 [`spawn_detached`]：用同样的命令行
//!   （去掉 `-d` / `--detach`）再起一个自己，新会话、脱离终端、输入输出丢掉（日志写文件），等它写出 pid 文件、活过一会儿才返回。
//!   Windows 上新进程有一个看不见的控制台（它再起的命令行子进程不弹窗口），并且尽量脱离父进程的作业对象
//!   （关掉 Windows Terminal、VS Code 的终端时不跟着结束）；父进程的作业不允许脱离时就留在作业里。
//!   后台进程挂在当前登录用户下：Windows 上用户注销时会被结束，要长期运行、开机自启要装成服务。
//! - 停：[`stop`] 让 pid 文件里的进程正常关闭、等它退出，和 Ctrl-C 一样走 [`Registry::run`](super::Registry::run) 的信号处理：
//!   - Unix：发 SIGTERM
//!   - Windows：没有 SIGTERM。[`Registry::run`](super::Registry::run) 监听命名管道 `\\.\pipe\hygiea-stop-<进程号>`，
//!     [`stop`] 连上它就是让它关闭。管道不分登录会话，从 ssh、另一个远程桌面会话里也能停；只有同一个用户、管理员能连上。
//!     对方没有这个管道（不是 hygiea 的应用）时才强制结束，组件来不及收尾
//! - 查：[`status`] 返回在跑的进程号。
//!
//! ```ignore
//! #[derive(clap::Parser)]
//! struct Cli {
//!     #[command(flatten)]
//!     config: ConfigArgs,
//!     #[command(flatten)]
//!     daemon: DaemonArgs,
//! }
//! let cli = Cli::parse();
//! let (registry, config) = Registry::load_config::<AppConfig>(&cli.config);
//! if cli.daemon.detach {
//!     let pid = daemon::spawn_detached(pid_file, Duration::from_secs(3), Duration::from_secs(30))?;
//!     return Ok(());
//! }
//! ```

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::app::AppErr;
use crate::{Result, bail, err};

/// 后台运行的命令行参数，`#[command(flatten)]` 合进自己的 CLI
#[derive(clap::Args, Debug, Clone, Default)]
#[command(about = None, long_about = None)]
pub struct DaemonArgs {
    /// 在后台跑：起好就返回，日志看配置里写的日志目录
    #[arg(short = 'd', long = "detach")]
    pub detach: bool,
}

/// 运行中的 pid 文件：写进自己的进程号，drop 时删掉（内容不是自己的进程号就不动）
pub(crate) struct PidFile {
    path: PathBuf,
}

impl PidFile {
    /// 文件里的进程还在：[`AppErr::AlreadyRunning`]；没有、或者进程已经没了：写进自己的进程号
    pub(crate) fn acquire(path: &Path) -> Result<PidFile> {
        if let Some(pid) = status(path) {
            bail!(AppErr::AlreadyRunning, { "pid": pid, "path": path.display().to_string() });
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).map_err(|e| failed(format!("create {}: {e}", dir.display())))?;
        }
        std::fs::write(path, std::process::id().to_string())
            .map_err(|e| failed(format!("write {}: {e}", path.display())))?;
        Ok(PidFile { path: path.to_path_buf() })
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if read_pid(&self.path) == Some(std::process::id())
            && let Err(e) = std::fs::remove_file(&self.path)
        {
            tracing::warn!("remove pid file {} failed: {e}", self.path.display());
        }
    }
}

/// pid 文件里的进程还在就是它的进程号；没有文件、写的不是数字、进程已经没了都是 `None`。
/// 自己的进程号不算（自己读到自己写的，不是「别的在跑」）
pub fn status(pid_file: &Path) -> Option<u32> {
    let pid = read_pid(pid_file)?;
    (pid != std::process::id() && exists(pid)).then_some(pid)
}

/// 让 pid 文件里的进程正常关闭（Unix 发 SIGTERM；Windows 连它的停止管道，对方没有管道时强制结束），等它退出，最多 `timeout`。
/// 没在跑返回 `Ok(None)`，停了返回它的进程号；超时报 [`AppErr::StopTimeout`]
pub fn stop(pid_file: &Path, timeout: Duration) -> Result<Option<u32>> {
    let Some(pid) = status(pid_file) else {
        return Ok(None);
    };
    #[cfg(windows)]
    if !request_stop(pid)? {
        terminate(pid)?;
    }
    #[cfg(not(windows))]
    terminate(pid)?;
    let started = Instant::now();
    while exists(pid) {
        if started.elapsed() >= timeout {
            bail!(AppErr::StopTimeout, { "pid": pid, "secs": timeout.as_secs() });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(Some(pid))
}

/// 后台再起一个自己：命令行是现在的去掉 `-d` / `--detach`（[`detach_args`]），新会话、输入输出丢掉。
/// 等它把自己的进程号写进 `pid_file`、再活过 `settle` 才返回它的进程号；中途退出、`timeout` 内没写出 pid 文件报 [`AppErr::ProcessFailed`]
pub fn spawn_detached(pid_file: &Path, settle: Duration, timeout: Duration) -> Result<u32> {
    let exe = std::env::current_exe().map_err(|e| failed(format!("current exe: {e}")))?;
    let args = detach_args(std::env::args_os().skip(1));
    let command = || {
        let mut command = std::process::Command::new(&exe);
        command
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    };
    let mut child = spawn_wrapped(command).map_err(|e| failed(format!("spawn: {e}")))?;
    let pid = child.id();
    let started = Instant::now();
    let mut written_at: Option<Instant> = None;
    loop {
        if let Some(code) = child.try_wait().map_err(|e| failed(format!("wait {pid}: {e}")))? {
            bail!(AppErr::ProcessFailed, format!("background process {pid} exited ({code}), see its logs"));
        }
        match written_at {
            Some(at) if at.elapsed() >= settle => return Ok(pid),
            Some(_) => {}
            None if read_pid(pid_file) == Some(pid) => written_at = Some(Instant::now()),
            None if started.elapsed() >= timeout => bail!(
                AppErr::ProcessFailed,
                format!("background process {pid} wrote no pid file {} in {}s", pid_file.display(), timeout.as_secs())
            ),
            None => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 去掉 `-d`、`--detach` 这两个参数（只认单独的一项，不拆 `-dv` 这种合写）
pub fn detach_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    args.into_iter()
        .filter(|arg| arg != "-d" && arg != "--detach")
        .collect()
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn failed(cause: String) -> crate::HyErr {
    err!(AppErr::ProcessFailed, cause)
}

/// 这台机器上有没有这个进程（sysinfo，跨平台）
fn exists(pid: u32) -> bool {
    process_of(pid).is_some()
}

/// 只刷新这一个进程，有就返回它
fn process_of(pid: u32) -> Option<sysinfo::System> {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new().ok()?;
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).is_some().then_some(system)
}

/// 结束进程（sysinfo）：Unix 发 SIGTERM，应用按 Ctrl-C 一样正常关闭；Windows 上是强制结束（组件来不及收尾），只在对方没有停止管道时用
fn terminate(pid: u32) -> Result<()> {
    let Some(system) = process_of(pid) else {
        return Ok(());
    };
    let Some(process) = system.process(sysinfo::Pid::from_u32(pid)) else {
        return Ok(());
    };
    let sent = match process.kill_with(sysinfo::Signal::Term) {
        Some(sent) => sent,
        None => process.kill(),
    };
    if !sent {
        bail!(AppErr::ProcessFailed, format!("terminate process {pid}"));
    }
    Ok(())
}

/// 用 process-wrap 起后台进程：新会话，脱离终端
#[cfg(unix)]
fn spawn_wrapped(
    command: impl Fn() -> std::process::Command,
) -> std::io::Result<Box<dyn process_wrap::std::ChildWrapper>> {
    let mut wrapped = process_wrap::std::CommandWrap::from(command());
    wrapped.wrap(process_wrap::std::ProcessSession);
    wrapped.spawn()
}

/// 用 process-wrap 起后台进程：看不见的控制台、自己一个进程组、脱离父进程的作业对象；
/// 作业不允许脱离（拒绝访问）时不带脱离再起一次
#[cfg(windows)]
fn spawn_wrapped(
    command: impl Fn() -> std::process::Command,
) -> std::io::Result<Box<dyn process_wrap::std::ChildWrapper>> {
    use windows::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
    const ERROR_ACCESS_DENIED: i32 = 5;
    let spawn = |flags| {
        let mut wrapped = process_wrap::std::CommandWrap::from(command());
        wrapped.wrap(process_wrap::std::CreationFlags(flags));
        wrapped.spawn()
    };
    let flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    match spawn(flags | CREATE_BREAKAWAY_FROM_JOB) {
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => spawn(flags),
        result => result,
    }
}

/// Windows 上让进程正常关闭用的命名管道：[`Registry::run`](super::Registry::run) 监听，[`stop`] 连
#[cfg(windows)]
fn stop_pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\hygiea-stop-{pid}")
}

/// 连 `pid` 的停止管道，让它正常关闭。连上了返回 `true`；管道不存在（不是 hygiea 的应用）返回 `false`；
/// 连不上（没权限等）报 [`AppErr::ProcessFailed`]。所有实例都忙时等一下再连，最多 5 秒
#[cfg(windows)]
fn request_stop(pid: u32) -> Result<bool> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let name = stop_pipe_name(pid);
    let started = Instant::now();
    loop {
        match std::fs::OpenOptions::new().write(true).open(&name) {
            Ok(_pipe) => return Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && started.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => bail!(AppErr::ProcessFailed, format!("connect {name}: {e}")),
        }
    }
}

/// Windows 上 [`Registry::run`](super::Registry::run) 监听的停止管道：有连接进来就是 [`stop`] 要它正常关闭
#[cfg(windows)]
pub(crate) struct StopPipe {
    name: String,
    /// 等下一个连接的实例；建不出新实例后是 `None`，之后不再收到
    server: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
}

#[cfg(windows)]
impl StopPipe {
    /// 建自己进程号的管道。要求是第一个实例：同名的已经被别的进程建了就报错，不和它共用
    pub(crate) fn listen() -> std::io::Result<StopPipe> {
        let name = stop_pipe_name(std::process::id());
        let server = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)?;
        Ok(StopPipe { name, server: Some(server) })
    }

    /// 等下一个连接。每来一个连接换一个新实例，后面的请求（关闭过程中再停一次）照样收得到
    pub(crate) async fn recv(&mut self) {
        // 客户端连上又马上断开、服务端还没开始等时，ConnectNamedPipe 报这个，也算来过一个连接
        const ERROR_NO_DATA: i32 = 232;
        loop {
            let Some(server) = self.server.as_mut() else {
                return std::future::pending().await;
            };
            let connected = match server.connect().await {
                Ok(()) => true,
                Err(e) if e.raw_os_error() == Some(ERROR_NO_DATA) => true,
                Err(e) => {
                    tracing::warn!("stop pipe {} accept failed: {e}", self.name);
                    false
                }
            };
            self.server = tokio::net::windows::named_pipe::ServerOptions::new()
                .create(&self.name)
                .inspect_err(|e| tracing::error!("stop pipe {}: create next instance failed: {e}", self.name))
                .ok();
            if connected {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 停止管道：连上就收到请求；收到后换了新实例，第二次也收得到；没有这个管道的进程号返回 false
    #[cfg(windows)]
    #[tokio::test]
    async fn stop_pipe_receives_requests() {
        let mut pipe = StopPipe::listen().unwrap();
        let pid = std::process::id();
        for _ in 0..2 {
            let client = tokio::task::spawn_blocking(move || request_stop(pid));
            tokio::time::timeout(Duration::from_secs(5), pipe.recv()).await.unwrap();
            assert!(client.await.unwrap().unwrap());
        }
        // 同一个进程号再建一个：不是第一个实例，报错
        assert!(StopPipe::listen().is_err());
        // 进程号不会是 u32::MAX（Windows 的进程号是 4 的倍数），没有这个管道
        assert!(!request_stop(u32::MAX).unwrap());
    }

    #[test]
    fn detach_args_drops_detach_flags() {
        let args = ["daemon", "run", "-d", "-f", "local", "--detach", "--set", "a=1"].map(OsString::from);
        assert_eq!(detach_args(args), ["daemon", "run", "-f", "local", "--set", "a=1"].map(OsString::from));
    }

    #[test]
    fn pid_file_written_and_removed_on_drop() {
        let dir = std::env::temp_dir().join(format!("hygiea-pid-{}", std::process::id()));
        let path = dir.join("app.pid");
        let guard = PidFile::acquire(&path).unwrap();
        assert_eq!(read_pid(&path), Some(std::process::id()));
        // 自己写的不算「别的在跑」
        assert_eq!(status(&path), None);
        drop(guard);
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stale_pid_file_is_not_running() {
        let dir = std::env::temp_dir().join(format!("hygiea-pid-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.pid");
        // 起一个马上退出的进程，拿它的进程号当旧文件
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        std::fs::write(&path, pid.to_string()).unwrap();
        assert_eq!(status(&path), None);
        let guard = PidFile::acquire(&path).unwrap();
        assert_eq!(read_pid(&path), Some(std::process::id()));
        drop(guard);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stop_terminates_running_process() {
        let dir = std::env::temp_dir().join(format!("hygiea-pid-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.pid");
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        std::fs::write(&path, child.id().to_string()).unwrap();
        assert_eq!(status(&path), Some(child.id()));
        // 别的进程在跑：再起一份报 AlreadyRunning
        let again = PidFile::acquire(&path).err().unwrap();
        assert!(again.is(AppErr::AlreadyRunning), "{again}");
        // sleep 收到 SIGTERM 就退出；子进程退出后要 wait 一下才从进程表里消失，放到另一个线程里收尸
        let reaper = std::thread::spawn(move || child.wait());
        assert!(stop(&path, Duration::from_secs(5)).unwrap().is_some());
        reaper.join().unwrap().unwrap();
        assert_eq!(stop(&path, Duration::from_secs(5)).unwrap(), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
