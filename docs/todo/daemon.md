# 守护进程（app-daemon）待办

`hygiea::app::daemon`（feature `app-daemon`）的用法和行为见它的模块文档。这里是还没做的。

## Windows 上正常停

现在 `daemon::stop` 在 Windows 上是强制结束（sysinfo 的 `kill`，底下是 `TerminateProcess`），组件来不及收尾：
Windows 没有 SIGTERM，`GenerateConsoleCtrlEvent` 只能发给同一个控制台里的进程，后台进程已经脱离了控制台，收不到。

两种做法，还没定：

- **做成 Windows 服务**：用 Mullvad 的 `windows-service`（或者微软的 `windows-services`），由服务管理器起停，停的时候服务收到「停止」通知，
  `Registry` 当成退出信号处理。Windows 上的标准做法，但要先注册服务、要管理员权限，和 Unix 上 `run -d` / `stop` 的用法不一样。
- **命名事件**：应用启动时建一个命名事件（名字按进程号），`Registry` 等退出信号时同时等它；`stop` 触发这个事件。
  用法和 Unix 一样、不要管理员权限，但是自己造的机制。

## Windows 上没验证过

`daemon.rs` 的 Windows 部分（process-wrap 的 `CreationFlags`、`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`）只照着库的文档写，
没在 Windows 上编译、运行过；测试用到的 `true`、`sleep` 是 Unix 命令，Windows 上要换写法。要在 Windows 上跑一遍 `app-daemon` 的测试，
以后加 CI 时带上 Windows。
