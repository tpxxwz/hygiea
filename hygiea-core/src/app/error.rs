//! app 的错误码

use crate::hy_err;

/// 应用框架和组件生命周期的错误，按先后分段：Registry `0xx`、检查配置 `1xx`、占用外部资源 `2xx`、关闭 `3xx`，通用的 `999`。
/// 和 [`BaseErr`](crate::BaseErr) 共用项目前缀 999，模块前缀是 02。
///
/// 组件直接用这里的变体，底层原因用 `wrap_err` / `with_source` 挂上：
///
/// ```ignore
/// TcpListener::bind(&addr).await.wrap_err(|| err!(BaseAppErr::BindFailed, &addr))?;
/// ```
#[derive(hy_err)]
#[err_code_internal_module_prefix = "001"]
pub enum BaseAppErr {
    // ---- 通用 ----
    /// 通用错误：现有变体都不合适时用，cause 写清楚是哪一步、什么问题，底层原因用 `wrap_err` 挂在 source 上。
    /// 外部组件不用等这里加专门的变体就能用；某类错误用多了，再加专门的变体。
    /// 和 [`BaseErr::SysErr`](crate::BaseErr::SysErr) 一样放在最前面、用段末的码，区别是带 cause，看得出哪一步出错
    #[error(err_code = "99", err_tpl = "Component error: {{ cause }}")]
    ComponentError,

    // ---- Registry：包装组件启动失败 ----
    /// 组件 startup 返回错误，Registry 带上是第几个、哪个组件，组件的错误挂在 source 上
    #[error(
        err_code = "01",
        err_tpl = "component[{{ index }}] {{ type_name }}({{ name }}) failed to start"
    )]
    ComponentStartFailed,
    /// 运行中组件的后台任务自己结束了（panic 或提前退出），应用已经不完整，Registry 关闭所有组件后返回它；
    /// panic 的原因挂在 source 上
    #[error(
        err_code = "02",
        err_tpl = "{{ type_name }}({{ name }}) background task {{ reason }} while running"
    )]
    TaskExited,
    /// Deferred 组件第二阶段 `activate` 返回错误，Registry 带上是第几个、哪个组件，组件的错误挂在 source 上
    #[error(
        err_code = "03",
        err_tpl = "component[{{ index }}] {{ type_name }}({{ name }}) failed to activate"
    )]
    ComponentActivateFailed,

    // ---- 启动前检查：配置、依赖关系 ----
    /// 必填配置没给，比如 AxumConfig.router、TonicConfig.serve_fn
    #[error(err_code = "11", err_tpl = "Config not set: {{ field }}")]
    ConfigMissing,
    /// 配置值不合法：地址解析不了、SQLite URL / 选项不对、redis mode 写错、cluster 没配节点
    #[error(err_code = "12", err_tpl = "Invalid config: {{ cause }}")]
    InvalidConfig,
    /// 依赖的资源没有：没有组件在 `provides` 里声明它（启动前检查），`require` 取不到，
    /// 或者组件声明了 `provides` 却没在 `startup` 里放进去
    #[error(err_code = "13", err_tpl = "Resource missing: {{ resource }}")]
    ResourceMissing,
    /// 组件之间的依赖成环，没法排出启动顺序；`cycle` 是环上的组件，比如 `A(a) -> B(b) -> A(a)`
    #[error(err_code = "14", err_tpl = "Component dependency cycle: {{ cycle }}")]
    DependencyCycle,
    /// 同一个资源有两个组件都声明了 `provides`，不知道依赖它的组件该排在谁后面
    #[error(
        err_code = "15",
        err_tpl = "Resource {{ resource }} provided by both {{ first }} and {{ second }}"
    )]
    DuplicateProvider,

    // ---- 组件启动第二步：占用外部资源 ----
    /// 监听端口失败（axum、tonic）
    #[error(err_code = "21", err_tpl = "Bind failed: {{ addr }}")]
    BindFailed,
    /// 连接外部服务失败（pg、sqlite、redis）
    #[error(err_code = "22", err_tpl = "Connect failed: {{ target }}")]
    ConnectFailed,
    /// 连上之后的其他初始化步骤失败，比如建表、跑迁移
    #[error(err_code = "23", err_tpl = "Component init failed: {{ cause }}")]
    InitFailed,

    // ---- 关闭 ----
    /// 组件 `stop` 收尾失败，比如关连接出错；只打 WARN，不影响关后面的组件
    #[error(err_code = "31", err_tpl = "Component stop failed: {{ cause }}")]
    StopFailed,
}
