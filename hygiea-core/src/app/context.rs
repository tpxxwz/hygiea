//! 进程级的信息：启动后不变、多个组件都可能用到，Registry 建好交给组件

/// 进程级的信息。Registry 按 [`RegistryConfig`](super::RegistryConfig) 建好，组件启动时拿到
/// （[`ConfigResource::from_config`](super::ConfigResource::from_config) 的参数）。
/// 不经过 Registry 时自己建，测试里用 `AppContext::default()`
#[derive(Clone, Debug, Default)]
pub struct AppContext {
    /// 应用名，来自 `[registry] app_name`（默认 `hygiea`，小写下划线）。不经过 Registry 自己建时 `default()` 是空的
    pub app_name: String,
}
