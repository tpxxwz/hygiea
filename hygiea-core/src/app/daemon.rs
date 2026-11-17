//! 进程控制（feature `app-daemon`）：pid 文件、后台运行、停掉在跑的、查状态。Linux、macOS、Windows 都支持：
//! 后台起用 process-wrap，判断进程在不在、结束进程用 sysinfo。Windows 没有 SIGTERM，[`stop`] 在 Windows 上是强制结束。
//!
//! - pid 文件：框架配置 `[registry] pid_file` 写了路径时，[`Registry::run`](super::Registry::run) 启动时写进自己的进程号，
//!   正常退出时删掉（只删自己写的）。启动时文件里的进程还在，报 [`AppErr::AlreadyRunning`]，同一个应用不会起两份；
//!   进程已经没了（上次被强杀）的旧文件直接覆盖。
//! - 后台运行：命令行合进 [`DaemonArgs`]（`-d` / `--detach`）。带了它时应用调 [`spawn_detached`]：用同样的命令行
//!   （去掉 `-d` / `--detach`）再起一个自己，新会话、脱离终端、输入输出丢掉（日志写文件），等它写出 pid 文件、活过一会儿才返回。
//! - 停：[`stop`] 结束 pid 文件里的进程、等它退出。Unix 发 SIGTERM，和 Ctrl-C 一样走正常关闭
//!   （[`Registry::run`](super::Registry::run) 的信号处理）；Windows 没有 SIGTERM，强制结束，组件来不及收尾。
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

/// 结束 pid 文件里的进程（Unix 发 SIGTERM 正常关闭，Windows 强制结束），等它退出，最多 `timeout`。没在跑返回 `Ok(None)`，停了返回它的进程号；
/// 超时报 [`AppErr::StopTimeout`]
pub fn stop(pid_file: &Path, timeout: Duration) -> Result<Option<u32>> {
    let Some(pid) = status(pid_file) else {
        return Ok(None);
    };
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
    let mut command = std::process::Command::new(exe);
    command
        .args(detach_args(std::env::args_os().skip(1)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
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

/// 结束进程（sysinfo）：Unix 发 SIGTERM，应用按 Ctrl-C 一样正常关闭；Windows 没有 SIGTERM，强制结束（组件来不及收尾）
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

/// 用 process-wrap 起后台进程：Unix 新会话（脱离终端），Windows 不挂在当前控制台上、自己一个进程组
fn spawn_wrapped(command: std::process::Command) -> std::io::Result<Box<dyn process_wrap::std::ChildWrapper>> {
    let mut command = process_wrap::std::CommandWrap::from(command);
    #[cfg(unix)]
    command.wrap(process_wrap::std::ProcessSession);
    #[cfg(windows)]
    command.wrap(process_wrap::std::CreationFlags(
        windows::Win32::System::Threading::DETACHED_PROCESS | windows::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
    ));
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

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
