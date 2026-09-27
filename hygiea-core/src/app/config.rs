//! 配置：命令行参数、框架级配置、按来源分层加载（见模块文档「配置从哪里来」）

use serde::Deserialize;

use super::Registry;
use crate::log::TracingConfig;

/// 读配置用的命令行参数。优先级和环境变量的写法见[模块文档](crate::app)。
///
/// 没有自己参数的应用用 [`ConfigArgs::from_cli`]；有的话用 `#[command(flatten)]` 合进自己的 CLI。
/// 测试里可以直接构造，不用伪造命令行。
///
/// 占用的参数：`--env`、`-f` / `--config`、`-d` / `--config-dir`、`--set`。合进自己的 CLI 时不要再定义
/// 同名的参数：clap 只在 debug 构建里检查重名（启动时 panic），release 构建不检查
#[derive(clap::Args, Debug, Clone, Default)]
// 上面的 rustdoc 是给看代码的人的，不要让 clap 拿去当 --help 里的程序说明
#[command(about = None, long_about = None)]
pub struct ConfigArgs {
    /// 环境名，读 <配置目录>/{ENV}.toml；不传时读环境变量 {PREFIX}_ENV（默认 HYGIEA_ENV），再没有就是 dev
    #[arg(long)]
    pub env: Option<String>,
    /// 额外的配置文件名，逗号分隔，按顺序覆盖：-f local,secret 读 <配置目录>/local.toml、<配置目录>/secret.toml
    #[arg(
        short = 'f',
        long = "config",
        value_name = "NAME",
        value_delimiter = ','
    )]
    pub configs: Vec<String>,
    /// 配置文件目录，覆盖代码里设的默认目录；都没有时是当前目录下的 config
    #[arg(short = 'd', long = "config-dir", value_name = "DIR")]
    pub config_dir: Option<String>,
    /// 覆盖单个配置项，优先级最高，可以写多次：--set registry.tracing.root_env_filter=debug
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = parse_override)]
    pub overrides: Vec<(String, String)>,
    /// 环境变量前缀，不是命令行参数，用 [`ConfigArgs::env_prefix`] 设置
    #[arg(skip)]
    env_prefix: Option<String>,
    /// 代码里设的默认配置目录，不是命令行参数，用 [`ConfigArgs::default_config_dir`] 设置
    #[arg(skip)]
    default_config_dir: Option<String>,
}

impl ConfigArgs {
    /// 默认的环境变量前缀
    pub const DEFAULT_ENV_PREFIX: &'static str = "HYGIEA";
    /// 默认环境
    pub const DEFAULT_ENV: &'static str = "dev";
    /// 默认的配置文件目录，相对当前目录
    pub const DEFAULT_CONFIG_DIR: &'static str = "config";

    /// 只解析框架的这几个参数（`--env`、`-f`、`--set`），给没有自己命令行参数的应用用。
    /// 出现别的参数会报错退出；要加自己的参数，把 `ConfigArgs` flatten 进自己的 CLI
    pub fn from_cli() -> Self {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            args: ConfigArgs,
        }
        <Cli as clap::Parser>::parse().args
    }

    /// 换成应用自己的环境变量前缀，比如 `"MYAPP"`：配置项读 `MYAPP__...`，环境名读 `MYAPP_ENV`
    pub fn env_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.env_prefix = Some(prefix.into());
        self
    }

    /// 设默认的配置文件目录，命令行 `-d` / `--config-dir` 传了时以命令行为准。
    /// 最终取值：`-d` → 这里设的 → [`DEFAULT_CONFIG_DIR`](Self::DEFAULT_CONFIG_DIR)。
    /// 相对路径按当前目录解析，不想依赖从哪里启动时传绝对路径，比如 `concat!(env!("CARGO_MANIFEST_DIR"), "/config")`
    pub fn default_config_dir(mut self, dir: impl Into<String>) -> Self {
        self.default_config_dir = Some(dir.into());
        self
    }

    fn dir(&self) -> &str {
        self.config_dir
            .as_deref()
            .or(self.default_config_dir.as_deref())
            .unwrap_or(Self::DEFAULT_CONFIG_DIR)
    }

    fn prefix(&self) -> &str {
        self.env_prefix
            .as_deref()
            .unwrap_or(Self::DEFAULT_ENV_PREFIX)
    }

    /// 环境名和它从哪来的，启动时打出来方便确认
    fn resolve_env(&self) -> (String, String) {
        if let Some(env) = &self.env {
            return (env.clone(), "--env".to_string());
        }
        let var = format!("{}_ENV", self.prefix());
        match std::env::var(&var) {
            Ok(env) if !env.is_empty() => (env, var),
            _ => (Self::DEFAULT_ENV.to_string(), "default".to_string()),
        }
    }
}

/// `--set` 的值必须是 `key=value`，格式不对时 clap 直接报错并给出提示
fn parse_override(raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((key, value)) if !key.trim().is_empty() => {
            Ok((key.trim().to_string(), value.to_string()))
        }
        _ => Err(format!("expected KEY=VALUE, got `{raw}`")),
    }
}

/// `Registry` 的框架级配置。
///
/// - `tracing`：`Registry` 拿到配置后立即按它初始化日志；不写时用默认值（只输出到控制台，级别 `info`）
/// - `shutdown_timeout_secs`：关闭时每个组件最多等多久，默认 15 秒
/// - `shutdown_delay_secs`：收到退出信号后先照常服务多久再开始关，默认 0（k8s 下一般配 5）
///
/// ```
/// use hygiea_core::app::RegistryConfig;
/// use hygiea_core::log::TracingConfig;
///
/// // 默认：只输出到控制台
/// let config = RegistryConfig::default();
///
/// // 自定义日志配置
/// let config = RegistryConfig {
///     tracing: TracingConfig { ..Default::default() },
///     ..Default::default()
/// };
/// ```
#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct RegistryConfig {
    pub tracing: TracingConfig,
    /// 关闭时**每个组件**最多等多久（秒），默认 15。组件可以用 [`Component::shutdown_timeout`](super::Component::shutdown_timeout)
    /// 单独指定（比如 HTTP 有长请求就给 30 秒）。超时的组件任务会被 abort，接着关下一个。
    pub shutdown_timeout_secs: u64,
    /// 收到退出信号后，先照常服务多久（秒）再开始关组件，默认 0。
    ///
    /// 给 k8s 用：Pod 收到 SIGTERM 的同时才开始从 Service 的 endpoints 里摘掉，负载均衡器生效要几秒，
    /// 这几秒里还会有新请求打过来。立刻停止接请求的话这些请求会失败，所以先等一会儿（一般 5 秒）。
    /// 和 k8s 的 `preStop: sleep 5` 作用一样，二选一即可。本地开发保持 0
    pub shutdown_delay_secs: u64,
}

impl RegistryConfig {
    pub const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 15;
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            tracing: TracingConfig::default(),
            shutdown_timeout_secs: Self::DEFAULT_SHUTDOWN_TIMEOUT_SECS,
            shutdown_delay_secs: 0,
        }
    }
}

/// 能提供 [`RegistryConfig`] 的配置类型。
///
/// 给应用的顶层配置实现这个 trait，[`Registry::load_config`] 读完配置文件后就能直接创建 Registry。
pub trait IntoRegistryConfig {
    fn registry_config(&self) -> RegistryConfig;
}

impl Registry {
    /// 按 `args` 读配置并创建 Registry，返回 `(registry, config)`，调用方还能继续用配置里的其他字段。
    ///
    /// 配置来源和优先级见[模块文档](crate::app)：`config/<env>.toml` < `-f` 额外文件 < 环境变量 < `--set`。
    /// `T` 要实现 [`IntoRegistryConfig`] 提供框架级配置。
    ///
    /// 配置读不到或解析失败是启动期错误，这时日志还没初始化：把本次的环境、工作目录、每个文件是否存在、
    /// 生效的环境变量名、`--set` 的 key、错误和它的原因链打到 stderr，然后 panic
    pub fn load_config<T>(args: &ConfigArgs) -> (Self, T)
    where
        T: serde::de::DeserializeOwned + IntoRegistryConfig,
    {
        let plan = ConfigPlan::new(args);
        plan.print();

        let mut builder = config::Config::builder();
        for file in &plan.files {
            builder = builder.add_source(config::File::with_name(file));
        }
        builder = builder.add_source(
            config::Environment::with_prefix(&plan.env_prefix)
                .prefix_separator("__")
                .separator("__")
                .try_parsing(true),
        );
        for (key, value) in &args.overrides {
            builder = builder
                .set_override(key, value.as_str())
                .unwrap_or_else(|e| plan.fail("apply --set", &e));
        }

        let config: T = builder
            .build()
            .unwrap_or_else(|e| plan.fail("load", &e))
            .try_deserialize()
            .unwrap_or_else(|e| plan.fail("parse", &e));

        let registry = Self::with_config(config.registry_config());
        (registry, config)
    }
}

/// 本次读配置用到的来源。启动时打到 stdout，出错时连同错误打到 stderr，排查时一眼能看出读了什么。
/// 只记录环境变量名和 `--set` 的 key，不记录值（值里可能有密码）
pub(super) struct ConfigPlan {
    env: String,
    /// 环境名从哪来：`--env`、`<PREFIX>_ENV` 或 `default`
    env_source: String,
    files: Vec<String>,
    env_prefix: String,
    /// 会参与合并的环境变量名（`<PREFIX>__` 开头的）
    env_vars: Vec<String>,
    override_keys: Vec<String>,
}

impl ConfigPlan {
    fn new(args: &ConfigArgs) -> Self {
        let (env, env_source) = args.resolve_env();
        let dir = args.dir();
        let files = std::iter::once(env.as_str())
            .chain(
                args.configs
                    .iter()
                    .map(|name| name.trim_end_matches(".toml")),
            )
            .map(|name| format!("{dir}/{name}.toml"))
            .collect();
        let env_prefix = args.prefix().to_string();
        // 和 config 库一样不区分大小写
        let var_prefix = format!("{env_prefix}__").to_lowercase();
        let mut env_vars: Vec<String> = std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .filter(|key| key.to_lowercase().starts_with(&var_prefix))
            .collect();
        env_vars.sort();
        Self {
            env,
            env_source,
            files,
            env_prefix,
            env_vars,
            override_keys: args.overrides.iter().map(|(key, _)| key.clone()).collect(),
        }
    }

    fn print(&self) {
        println!("Environment: [{}] (from {})", self.env, self.env_source);
        for file in &self.files {
            println!("Loading config: [{file}]");
        }
        if !self.env_vars.is_empty() {
            println!("Env overrides: {}", self.env_vars.join(", "));
        }
        if !self.override_keys.is_empty() {
            println!("--set overrides: {}", self.override_keys.join(", "));
        }
    }

    /// 配置读取失败：日志还没初始化，把排查需要的信息都打到 stderr，然后 panic。
    ///
    /// `stage`：`apply --set`（key 写法不对）、`load`（读文件、toml 语法）、
    /// `parse`（合并后反序列化成 `T`，错误里会带出错的 key 和来源）
    fn fail(&self, stage: &str, err: &config::ConfigError) -> ! {
        eprintln!("ERROR hygiea: failed to {stage} config");
        eprintln!("  env: {} (from {})", self.env, self.env_source);
        match std::env::current_dir() {
            Ok(dir) => eprintln!("  working dir: {}", dir.display()),
            Err(e) => eprintln!("  working dir: <unknown: {e}>"),
        }
        eprintln!("  sources (later ones override earlier ones):");
        for file in &self.files {
            let state = if std::path::Path::new(file).is_file() {
                "found"
            } else {
                "NOT FOUND"
            };
            eprintln!("    file {file} [{state}]");
        }
        if self.env_vars.is_empty() {
            eprintln!("    env  {}__* [none]", self.env_prefix);
        } else {
            eprintln!("    env  {}", self.env_vars.join(", "));
        }
        if !self.override_keys.is_empty() {
            eprintln!("    --set {}", self.override_keys.join(", "));
        }
        eprintln!("  error: {err}");
        let mut source = std::error::Error::source(err);
        while let Some(cause) = source {
            eprintln!("  caused by: {cause}");
            source = cause.source();
        }
        panic!("failed to {stage} config: {err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_dir() {
        let args = ConfigArgs {
            env: Some("prod".to_string()),
            configs: vec!["local".to_string(), "secret.toml".to_string()],
            ..Default::default()
        };
        // 默认相对当前目录的 config
        let plan = ConfigPlan::new(&args);
        assert_eq!(
            plan.files,
            [
                "config/prod.toml",
                "config/local.toml",
                "config/secret.toml"
            ]
        );
        // 换目录后环境文件和 -f 的文件都从这个目录读
        let args = args.default_config_dir("/etc/myapp");
        let plan = ConfigPlan::new(&args);
        assert_eq!(
            plan.files,
            [
                "/etc/myapp/prod.toml",
                "/etc/myapp/local.toml",
                "/etc/myapp/secret.toml"
            ]
        );
        // 命令行 -d 覆盖代码里设的默认目录
        let args = ConfigArgs {
            config_dir: Some("./cfg".to_string()),
            ..args
        };
        assert_eq!(ConfigPlan::new(&args).files[0], "./cfg/prod.toml");
    }

    #[test]
    fn parse_override_normal() {
        assert_eq!(
            parse_override("key=value").unwrap(),
            ("key".to_string(), "value".to_string())
        );
    }

    #[test]
    fn parse_override_trims_key_but_not_value() {
        // 只 trim key，value 原样保留（值里可能有意义的前后空格，比如格式串）
        assert_eq!(
            parse_override(" key = value ").unwrap(),
            ("key".to_string(), " value ".to_string())
        );
    }

    #[test]
    fn parse_override_value_can_contain_eq() {
        // split_once 只切第一个 `=`，剩下的都算 value
        assert_eq!(
            parse_override("key=a=b").unwrap(),
            ("key".to_string(), "a=b".to_string())
        );
    }

    #[test]
    fn parse_override_empty_key_is_error() {
        assert!(parse_override("=v").is_err());
    }

    #[test]
    fn parse_override_no_eq_is_error() {
        assert!(parse_override("noeq").is_err());
    }

    #[test]
    fn resolve_env_prefers_cli_flag() {
        // --env 优先，不去看环境变量（不设置 env 就不用碰 std::env::set_var，
        // 走这个分支不用管 HYGIEA_ENV 到底是什么）
        let args = ConfigArgs {
            env: Some("staging".to_string()),
            ..Default::default()
        };
        assert_eq!(
            args.resolve_env(),
            ("staging".to_string(), "--env".to_string())
        );
    }

    #[test]
    fn registry_config_default_values() {
        let config = RegistryConfig::default();
        assert_eq!(config.shutdown_timeout_secs, 15);
        assert_eq!(config.shutdown_delay_secs, 0);
        assert_eq!(config.tracing.root_env_filter, "info");
        assert_eq!(config.tracing.root_dir, "./logs");
        assert!(!config.tracing.utc_time);
        assert!(config.tracing.layers.is_empty());
    }

    #[test]
    fn registry_config_rejects_unknown_field() {
        let result: Result<RegistryConfig, _> = serde_json::from_str(r#"{"unknown_field": 1}"#);
        assert!(result.is_err());
    }

    #[test]
    fn registry_config_missing_fields_use_default() {
        // deny_unknown_fields 只挡未知字段，缺省字段照样走 default
        let config: RegistryConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.shutdown_timeout_secs, 15);
    }
}
