//! 日志：tracing 的配置和初始化，不依赖组件框架。
//!
//! # 测试注意
//!
//! [`init`] 装的是进程级的全局 subscriber，一个进程只能装一次、装上就卸不掉；`cargo test` 又把同一个
//! 测试文件里的测试放在一个进程里并行跑。所以：
//!
//! - **普通测试不要依赖 tracing 配置**：同一个进程里只有第一次 init 生效，是哪个测试先跑并不固定。
//!   要检查"代码打了什么日志"，用 `hygiea-test-support` 的 `logs::capture()`，它只作用于当前线程；
//! - **测 tracing 配置本身**（文件写没写、时间格式、级别过滤）：每种配置单独一个 `tests/*.rs` 文件
//!   （每个文件编译成独立的进程），文件里只放一个 `#[test]` 按顺序检查，见 `tests/log_*.rs`。
//!
//! 用组件框架时由 `app::Registry` 在拿到配置后直接初始化，不用自己调。

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing_appender::non_blocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_log::LogTracer;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt};

use crate::datetime::{DateTimeFormatter, now, now_local};
use crate::{HyErr, ResultExt, err, hy_err};

// ---- errors ----------------------------------------------------------------

/// 日志初始化的错误，按 `init` 里出错的先后排。和 [`BaseErr`](crate::BaseErr) 共用项目前缀 999，
/// 模块前缀是 03；原始错误挂在 source 上
#[derive(hy_err)]
#[err_code_module_prefix = "03"]
pub enum BaseLogErr {
    /// 建不了日志文件，比如目录没有写权限
    #[error(
        err_code = "002",
        err_tpl = "Create log file appender failed: {{ dir }}"
    )]
    AppenderFailed,
    /// 装全局 logger / subscriber 失败，一般是同一个进程里已经装过
    #[error(err_code = "003", err_tpl = "Install global logger failed")]
    InstallFailed,
    /// 同一目录下两个 layer 的文件名重叠（一个的「前缀 + 后缀」能匹配到另一个的文件），清理旧文件时会互删
    #[error(
        err_code = "004",
        err_tpl = "Log layers {{ a }} and {{ b }} have overlapping file names in the same dir"
    )]
    FileNameOverlap,
    /// filter 不符合 tracing 的语法，比如级别写错（`app_config=infoo`）
    #[error(err_code = "005", err_tpl = "Invalid log filter: {{ filter }}")]
    InvalidFilter,
    /// 控制台关了（`console.disable = true`），又没有启用的文件 layer，日志没有任何输出
    #[error(
        err_code = "006",
        err_tpl = "No log output: console is disabled and no file layer is enabled"
    )]
    NoOutput,
}

// ---- config types ----------------------------------------------------------

#[derive(Deserialize, Clone, Default, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct ConsoleLayer {
    /// 关掉控制台输出。关了之后至少要有一个启用的文件 layer，否则 init 报 [`BaseLogErr::NoOutput`]
    pub disable: bool,
    pub env_filter: String,
    /// 是否输出 ANSI 颜色。配了就按配置来；不配时自动判断：stdout 是终端且没设 `NO_COLOR` 才输出，
    /// 被 docker、journald、重定向收集时不输出
    pub ansi: Option<bool>,
}

impl ConsoleLayer {
    fn effective_ansi(&self) -> bool {
        use std::io::IsTerminal;
        self.ansi.unwrap_or_else(|| {
            std::io::stdout().is_terminal()
                && std::env::var("NO_COLOR").map_or(true, |v| v.is_empty())
        })
    }
}

/// 日志文件的滚动周期，对应 tracing-appender 的 [`Rotation`]（它没实现 `Deserialize`，所以自己定义一个）。
/// 配置里写变体名，区分大小写：`rolling = "Daily"`；写错时加载配置就报错
#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rolling {
    Minutely,
    Hourly,
    Daily,
    Weekly,
    #[default]
    Never,
}

impl From<Rolling> for Rotation {
    fn from(rolling: Rolling) -> Self {
        match rolling {
            Rolling::Minutely => Rotation::MINUTELY,
            Rolling::Hourly => Rotation::HOURLY,
            Rolling::Daily => Rotation::DAILY,
            Rolling::Weekly => Rotation::WEEKLY,
            Rolling::Never => Rotation::NEVER,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct FileLayer {
    pub disable: bool,
    pub env_filter: String,
    /// 日志目录，拼在 [`TracingConfig::root_dir`] 下面；默认空，直接写在 root_dir 里。
    /// 写绝对路径时不拼 root_dir（`Path::join` 的规则）
    pub dir: String,
    pub filename_prefix: String,
    pub filename_suffix: String,
    /// 默认 true：用 tracing-appender 的 `non_blocking`（默认参数），后台线程写文件，业务线程不等磁盘；
    /// 队列上限 12.8 万行，满了新日志直接丢弃，不报错。false：在打日志的线程里同步写，不丢日志，
    /// 但磁盘慢时会卡住 tokio worker 线程
    pub non_blocking: bool,
    pub rolling: Rolling,
    /// 最多保留几个日志文件，默认 None 不清理（和 tracing-appender 一致）。
    /// 配了 n 时，建 appender 和每次滚动时把同目录下「前缀 + 后缀」匹配的文件删到只剩 n-1 个；
    /// rolling 为 Never 时只有一个文件，配 1 等于每次启动清空
    pub max_log_files: Option<usize>,
}

impl FileLayer {
    pub const DEFAULT_FILENAME_PREFIX: &'static str = "app";
    pub const DEFAULT_FILENAME_SUFFIX: &'static str = "log";
}

impl Default for FileLayer {
    fn default() -> Self {
        Self {
            disable: false,
            env_filter: String::new(),
            dir: String::new(),
            filename_prefix: FileLayer::DEFAULT_FILENAME_PREFIX.to_string(),
            filename_suffix: FileLayer::DEFAULT_FILENAME_SUFFIX.to_string(),
            non_blocking: true,
            rolling: Rolling::Never,
            max_log_files: None,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct TracingConfig {
    pub console: ConsoleLayer,
    pub layers: Vec<FileLayer>,
    pub root_env_filter: String,
    /// 所有文件 layer 共用的日志根目录，默认 `./logs`，空字符串也按默认处理
    pub root_dir: String,
    /// 日志时间的格式，默认 `WithOffset.YmdTHMS3F`
    pub time_format: DateTimeFormatter,
    /// 日志时间用 UTC；默认 false，用系统时区（TZ 环境变量 → 系统设置 → UTC）
    pub utc_time: bool,
}

impl TracingConfig {
    pub const DEFAULT_ROOT_ENV_FILTER: &'static str = "info";
    pub const DEFAULT_ROOT_DIR: &'static str = "./logs";
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            console: ConsoleLayer::default(),
            layers: Vec::new(),
            root_env_filter: TracingConfig::DEFAULT_ROOT_ENV_FILTER.to_string(),
            root_dir: TracingConfig::DEFAULT_ROOT_DIR.to_string(),
            time_format: DateTimeFormatter::default(),
            utc_time: false,
        }
    }
}

// ---- init ------------------------------------------------------------------

/// 持有非阻塞文件写入的 guard，drop 时把缓冲里的日志刷出去，所以要一直持有到进程结束
pub struct LogGuard(#[allow(dead_code)] Vec<WorkerGuard>);

/// 按配置安装全局 subscriber，并把 `log` crate 的日志桥接进来。一个进程只能成功调用一次，
/// 再调用返回 [`BaseLogErr::InstallFailed`]。
///
/// 日志由这里统一接管：调用之前不要自己装别的 tracing subscriber 或 `log` logger（比如 `env_logger::init()`），
/// 否则同样返回 [`BaseLogErr::InstallFailed`]。
///
/// 用 [`crate::app::Registry`] 时不用自己调，它拿到配置后会立即按配置初始化；
/// 不用组件框架的程序（脚本、CLI）直接调这个，返回的 guard 要一直持有
pub fn init(cfg: &TracingConfig) -> Result<LogGuard, HyErr> {
    init_tracing(cfg).map(LogGuard)
}

/// 用默认配置初始化：只输出到控制台，级别 `info`，
/// 本地时间。脚本、小工具里一行搞定：`let _guard = hygiea::log::init_default()?;`
pub fn init_default() -> Result<LogGuard, HyErr> {
    init(&TracingConfig::default())
}

#[derive(Clone)]
struct LogTime {
    format: DateTimeFormatter,
    utc: bool,
}

impl FormatTime for LogTime {
    fn format_time(&self, writer: &mut Writer<'_>) -> std::fmt::Result {
        let now = if self.utc { now() } else { now_local() };
        let timestamp = self.format.format(&now).map_err(|_| std::fmt::Error)?;
        writer.write_str(&timestamp)
    }
}

/// 本进程里 [`init`] 是否已经装过（或正在装）。先抢这个标记再建文件 appender：
/// 建 appender 会创建日志文件、按 max_log_files 清理旧文件，已经装过时这次配置反正不生效，不能留下这些副作用。
/// tracing 没有公开的"全局 subscriber 装过没有"的查询（`has_been_set` 连线程级的 `set_default` 也算），所以自己记
static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn init_tracing(cfg: &TracingConfig) -> Result<Vec<WorkerGuard>, HyErr> {
    use std::sync::atomic::Ordering;
    if INSTALLED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(err!(BaseLogErr::InstallFailed));
    }
    // 配置有问题（文件名重叠、目录建不了）没装上时把标记还回去，改好配置还能再 init；
    // 这次新建的日志目录也删掉，原本就有的目录不动
    let mut created_dirs = Vec::new();
    let result = build_and_install(cfg, &mut created_dirs);
    if result.is_err() {
        for dir in created_dirs.iter().rev() {
            // 尽力清理，删不掉也不能盖掉真正的初始化错误
            let _ = std::fs::remove_dir_all(dir);
        }
        INSTALLED.store(false, Ordering::Release);
    }
    result
}

fn build_and_install(
    cfg: &TracingConfig,
    created_dirs: &mut Vec<PathBuf>,
) -> Result<Vec<WorkerGuard>, HyErr> {
    let timer = LogTime {
        format: cfg.time_format,
        utc: cfg.utc_time,
    };

    let effective_root_filter = if cfg.root_env_filter.is_empty() {
        TracingConfig::DEFAULT_ROOT_ENV_FILTER
    } else {
        &cfg.root_env_filter
    };
    // 建目录之前先把所有 filter 校验一遍，写错时不留副作用
    parse_filter(effective_root_filter)?;
    parse_filter(&cfg.console.env_filter)?;
    for layer in cfg.layers.iter().filter(|l| !l.disable) {
        parse_filter(&layer.env_filter)?;
    }

    let root_dir = if cfg.root_dir.is_empty() {
        TracingConfig::DEFAULT_ROOT_DIR
    } else {
        &cfg.root_dir
    };
    check_file_overlap(root_dir, &cfg.layers, created_dirs)?;

    let mut work_guards: Vec<WorkerGuard> = Vec::new();

    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::new();

    // Console layer
    if !cfg.console.disable {
        let filter_str = if cfg.console.env_filter.is_empty() {
            effective_root_filter
        } else {
            &cfg.console.env_filter
        };
        let console_layer = fmt::layer()
            .with_timer(timer.clone())
            .with_ansi(cfg.console.effective_ansi())
            .with_target(true)
            .with_filter(parse_filter(filter_str)?);
        layers.push(Box::new(console_layer));
    }

    // File layers
    for layer_cfg in &cfg.layers {
        if layer_cfg.disable {
            continue;
        }
        let mut builder = RollingFileAppender::builder()
            .rotation(layer_cfg.rolling.into())
            .filename_prefix(&layer_cfg.filename_prefix)
            .filename_suffix(&layer_cfg.filename_suffix);
        if let Some(n) = layer_cfg.max_log_files {
            builder = builder.max_log_files(n);
        }
        let dir = layer_dir(root_dir, layer_cfg);
        let appender = builder
            .build(&dir)
            .wrap_err(|| err!(BaseLogErr::AppenderFailed, dir.display().to_string()))?;

        let filter_str = if layer_cfg.env_filter.is_empty() {
            effective_root_filter
        } else {
            &layer_cfg.env_filter
        };
        let file_filter = parse_filter(filter_str)?;

        // layer 本来就装在 Box<dyn Layer> 里动态分发，writer 也用类型擦除的 BoxMakeWriter，两种写法共用一份代码
        let writer = if layer_cfg.non_blocking {
            let (writer, guard) = non_blocking(appender);
            work_guards.push(guard);
            BoxMakeWriter::new(writer)
        } else {
            BoxMakeWriter::new(appender)
        };
        let file_layer = fmt::layer()
            .with_timer(timer.clone())
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(file_filter);
        layers.push(Box::new(file_layer));
    }

    // 控制台是使用者主动关的，又没有文件 layer：当成配置写漏报错，不偷偷退回控制台输出
    if layers.is_empty() {
        return Err(err!(BaseLogErr::NoOutput));
    }

    LogTracer::init().wrap_err(|| err!(BaseLogErr::InstallFailed))?;

    let subscriber = Registry::default().with(layers);
    tracing::subscriber::set_global_default(subscriber)
        .wrap_err(|| err!(BaseLogErr::InstallFailed))?;
    Ok(work_guards)
}

/// 按 tracing 的语法解析 filter，解析失败时原始错误挂在 source 上。
/// 注意 tracing 会把不带 `=` 的非级别名（比如 `infoo`）当成 target 名，这种能解析成功，不报错
fn parse_filter(filter: &str) -> Result<EnvFilter, HyErr> {
    EnvFilter::try_new(filter).wrap_err(|| err!(BaseLogErr::InvalidFilter, filter))
}

/// 同一目录下两个 layer 的文件名不能重叠：tracing-appender 清理旧文件只按目录 +「以前缀开头、以后缀结尾」匹配
/// （前缀后缀都为空时只匹配纯日期文件名），不区分是哪个 appender 建的。不管配没配 max_log_files 都检查，
/// 免得哪天给其中一个加上清理就删到另一个的文件。
///
/// 目录先建出来再 canonicalize（appender 反正也要建），符号链接、`..`、相对 / 绝对路径混用都能认出是同一个目录
fn check_file_overlap(
    root_dir: &str,
    layers: &[FileLayer],
    created_dirs: &mut Vec<PathBuf>,
) -> Result<(), HyErr> {
    let mut enabled: Vec<(usize, &FileLayer, PathBuf)> = Vec::new();
    for (i, l) in layers.iter().enumerate().filter(|(_, l)| !l.disable) {
        let dir = layer_dir(root_dir, l);
        let real = create_dir_tracked(&dir, created_dirs)
            .and_then(|_| std::fs::canonicalize(&dir))
            .wrap_err(|| err!(BaseLogErr::AppenderFailed, dir.display().to_string()))?;
        enabled.push((i, l, real));
    }
    for (k, (i, a, a_dir)) in enabled.iter().enumerate() {
        for (j, b, b_dir) in &enabled[k + 1..] {
            if a_dir == b_dir && (name_matches(a, b) || name_matches(b, a)) {
                return Err(err!(BaseLogErr::FileNameOverlap, {
                    "a": layer_name(*i, a_dir, a),
                    "b": layer_name(*j, b_dir, b),
                }));
            }
        }
    }
    Ok(())
}

/// b 生成的文件名（`前缀.日期.后缀`，空的部分省略）是否满足 a 的清理匹配规则
fn name_matches(a: &FileLayer, b: &FileLayer) -> bool {
    let (ap, as_) = (&a.filename_prefix, &a.filename_suffix);
    let (bp, bs) = (&b.filename_prefix, &b.filename_suffix);
    if ap.is_empty() && as_.is_empty() {
        // a 只匹配能按日期解析的文件名，b 也没有前缀后缀时才会撞上
        return bp.is_empty() && bs.is_empty();
    }
    (ap.is_empty() || bp.starts_with(ap.as_str())) && (as_.is_empty() || bs.ends_with(as_.as_str()))
}

/// 建目录，并把这次新建出来的最上层目录记到 created_dirs，init 失败时删掉它就能恢复原样
fn create_dir_tracked(dir: &Path, created_dirs: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let topmost_missing = dir
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .take_while(|p| !p.exists())
        .last()
        .map(Path::to_path_buf);
    std::fs::create_dir_all(dir)?;
    if let Some(top) = topmost_missing {
        created_dirs.push(top);
    }
    Ok(())
}

/// layer 实际写入的目录：root_dir 拼上 layer 的 dir，dir 为空时就是 root_dir
fn layer_dir(root_dir: &str, layer: &FileLayer) -> PathBuf {
    Path::new(root_dir).join(&layer.dir)
}

fn layer_name(index: usize, dir: &Path, layer: &FileLayer) -> String {
    format!(
        "#{index}({}*{})",
        dir.join(&layer.filename_prefix).display(),
        layer.filename_suffix
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format_with(utc: bool) -> String {
        let timer = LogTime {
            format: DateTimeFormatter::default(),
            utc,
        };
        let mut out = String::new();
        timer.format_time(&mut Writer::new(&mut out)).unwrap();
        out
    }

    #[test]
    fn test_tracing_config_deserialize() {
        let cfg: TracingConfig = serde_json::from_str("{}").unwrap();
        assert!(!cfg.utc_time);
        assert_eq!(cfg.time_format, DateTimeFormatter::default());

        let cfg: TracingConfig =
            serde_json::from_str(r#"{"utc_time": true, "time_format": "WithoutOffset.YmdHMS"}"#)
                .unwrap();
        assert!(cfg.utc_time);
        assert_eq!(
            cfg.time_format,
            "WithoutOffset.YmdHMS".parse::<DateTimeFormatter>().unwrap()
        );
    }

    #[test]
    fn test_log_time_utc_or_local() {
        // 默认格式 WithOffset.YmdTHMS3F，第 23 位之后是 offset
        assert!(format_with(true).ends_with("+00:00"));
        let local = DateTimeFormatter::default().format(&now_local()).unwrap();
        assert_eq!(format_with(false)[23..], local[23..]);
    }

    fn file_layer(dir: &str) -> TracingConfig {
        TracingConfig {
            layers: vec![FileLayer {
                dir: dir.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn test_parse_filter() {
        for ok in [
            "",
            "info",
            "warn,app_config=info",
            "my::mod=debug",
            "[span]=debug",
        ] {
            assert!(parse_filter(ok).is_ok(), "{ok:?}");
        }
        // 按 tracing 的规则，infoo 是 target 名，不报错
        assert!(parse_filter("infoo").is_ok());
        for bad in ["warn,app_config=infoo", "=info", "[span"] {
            let err = parse_filter(bad).err().unwrap();
            assert!(err.is(BaseLogErr::InvalidFilter), "{bad:?}");
            assert!(err.to_string().contains(bad));
            assert!(std::error::Error::source(&err).is_some(), "{bad:?}");
        }
    }

    // ---- init 失败：都在装全局 subscriber 之前失败，不会真的装上，所以能放在单元测试里 ----
    // 两个测试都会短暂占用 INSTALLED 标记，并行跑会互相看到对方占着、报成 InstallFailed，所以串行

    #[test]
    #[serial_test::serial(log_init)]
    fn test_init_invalid_filter() {
        // filter 在建目录之前校验，失败时目录不会被建出来；根目录本身也是这次新建的，还没建出来
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        let mut cfg = file_layer("");
        cfg.root_dir = root.to_str().unwrap().to_string();
        cfg.layers[0].env_filter = "app_config=infoo".to_string();
        let err = init(&cfg).err().unwrap();
        assert!(err.is(BaseLogErr::InvalidFilter));
        assert!(!root.exists());
        assert!(!INSTALLED.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    #[serial_test::serial(log_init)]
    fn test_init_no_output() {
        let mut cfg = TracingConfig::default();
        cfg.console.disable = true;
        assert!(init(&cfg).err().unwrap().is(BaseLogErr::NoOutput));
        // 文件 layer 全部 disable 也一样
        cfg.layers = vec![FileLayer {
            disable: true,
            ..Default::default()
        }];
        assert!(init(&cfg).err().unwrap().is(BaseLogErr::NoOutput));
        assert!(!INSTALLED.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn test_console_ansi() {
        let console: ConsoleLayer = serde_json::from_str("{}").unwrap();
        assert_eq!(console.ansi, None);
        // 配了就按配置，不看终端和 NO_COLOR
        let console: ConsoleLayer = serde_json::from_str(r#"{"ansi": true}"#).unwrap();
        assert!(console.effective_ansi());
        let console: ConsoleLayer = serde_json::from_str(r#"{"ansi": false}"#).unwrap();
        assert!(!console.effective_ansi());
    }

    #[test]
    fn test_deny_unknown_fields() {
        // 不认识的键直接报错，不静默忽略
        let err = serde_json::from_str::<FileLayer>(r#"{"roling": "Daily"}"#)
            .err()
            .unwrap();
        assert!(err.to_string().contains("unknown field `roling`"), "{err}");
        assert!(serde_json::from_str::<ConsoleLayer>(r#"{"color": true}"#).is_err());
        assert!(serde_json::from_str::<TracingConfig>(r#"{"root_filter": "info"}"#).is_err());
    }

    #[test]
    fn test_rolling_config() {
        let layer: FileLayer = serde_json::from_str("{}").unwrap();
        assert_eq!(layer.rolling, Rolling::Never);
        let layer: FileLayer = serde_json::from_str(r#"{"rolling": "Daily"}"#).unwrap();
        assert_eq!(layer.rolling, Rolling::Daily);
        // 区分大小写，写错在反序列化时就报错
        assert!(serde_json::from_str::<FileLayer>(r#"{"rolling": "daily"}"#).is_err());
        assert!(serde_json::from_str::<FileLayer>(r#"{"rolling": "Yearly"}"#).is_err());
    }

    #[test]
    #[serial_test::serial(log_init)]
    fn test_init_overlap_keeps_preexisting_root_dir() {
        // 根目录原本就有，只有 a/b 是这次新建的；两个 layer 文件名重叠，init 失败
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = TracingConfig {
            root_dir: root.to_str().unwrap().to_string(),
            layers: vec![layer("a/b", "app"), layer("a/b", "app-error")],
            ..Default::default()
        };
        let err = init(&cfg).err().unwrap();
        // 新建的从最上层 a 开始整个删掉，原本就有的根目录不动
        assert!(root.exists());
        assert!(!root.join("a").exists());
        assert!(err.is(BaseLogErr::FileNameOverlap));
        // 失败后标记已经还回去，没有占着
        assert!(!INSTALLED.load(std::sync::atomic::Ordering::Acquire));
    }

    fn layer(dir: &str, prefix: &str) -> FileLayer {
        FileLayer {
            dir: dir.to_string(),
            filename_prefix: prefix.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_layer_dir() {
        // dir 不配就是 root_dir，相对路径拼在 root_dir 下，绝对路径不拼
        assert_eq!(layer_dir("./logs", &layer("", "app")), Path::new("./logs"));
        assert_eq!(
            layer_dir("./logs", &layer("error", "app")),
            Path::new("./logs/error")
        );
        assert_eq!(
            layer_dir("./logs", &layer("/var/log/x", "app")),
            Path::new("/var/log/x")
        );
    }

    #[test]
    fn test_check_file_overlap() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path();
        let root_str = root.to_str().unwrap().to_string();
        let ok =
            |layers: &[FileLayer]| check_file_overlap(&root_str, layers, &mut Vec::new()).is_ok();
        // 前缀互相包含就报错，和顺序、max_log_files 都无关；dir 不配和配成 "." 是同一个目录
        assert!(!ok(&[layer("", "app"), layer("", "app-error")]));
        assert!(!ok(&[layer("", "apperror"), layer(".", "app")]));
        assert!(!ok(&[layer("", "app"), layer("", "app")]));
        let mut pruning = layer("", "app-error");
        pruning.max_log_files = Some(2);
        assert!(!ok(&[layer("", "app"), pruning]));
        // 前缀不重叠、目录不同：没问题
        assert!(ok(&[layer("", "app-main"), layer("", "app-error")]));
        assert!(ok(&[layer("a", "app"), layer("b", "app-error")]));
        assert!(ok(&[layer("", "app"), layer("error", "app-error")]));
        // 绝对路径、`..`、符号链接指向同一个目录也能认出来
        assert!(!ok(&[layer("", "app"), layer(&root_str, "app-error")]));
        assert!(!ok(&[layer("", "app"), layer("x/..", "app-error")]));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root, root.join("link")).unwrap();
            assert!(!ok(&[layer("", "app"), layer("link", "app-error")]));
        }
        // 被 disable 的 layer 不算
        let mut disabled = layer("", "app-error");
        disabled.disable = true;
        assert!(ok(&[layer("", "app"), disabled]));
        // 后缀也参与匹配：app*log 和 app-error*txt 不重叠
        let mut txt = layer("", "app-error");
        txt.filename_suffix = "txt".to_string();
        assert!(ok(&[layer("", "app"), txt]));

        let err = check_file_overlap(
            &root_str,
            &[layer("", "app"), layer("", "app-error")],
            &mut Vec::new(),
        )
        .err()
        .unwrap();
        assert!(err.is(BaseLogErr::FileNameOverlap));
        // 报错里是 canonicalize 之后的目录
        let real = std::fs::canonicalize(root).unwrap();
        let expect = |i: usize, prefix: &str| format!("#{i}({}*log)", real.join(prefix).display());
        assert!(err.to_string().contains(&expect(0, "app")), "{err}");
        assert!(err.to_string().contains(&expect(1, "app-error")), "{err}");
    }

    #[test]
    #[serial_test::serial(log_init)]
    fn test_init_overlap_removes_new_root_dir() {
        // 根目录本身也是这次新建的，两个 layer 文件名重叠，init 失败
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        let cfg = TracingConfig {
            root_dir: root.to_str().unwrap().to_string(),
            layers: vec![layer("", "app"), layer("", "app-error")],
            ..Default::default()
        };
        let err = init(&cfg).err().unwrap();
        assert!(err.is(BaseLogErr::FileNameOverlap));
        // 校验时新建的目录，连根目录一起删掉了
        assert!(!root.exists());
        assert!(!INSTALLED.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    #[serial_test::serial(log_init)]
    fn test_init_disabled_layer_filter_not_validated() {
        // disable 的 layer 不会走 parse_filter 校验，filter 写错也不报 InvalidFilter；
        // 控制台也关掉，凑成 NoOutput，证明确实是"跳过校验"而不是"凑巧建成功了"
        let mut cfg = TracingConfig::default();
        cfg.console.disable = true;
        cfg.layers = vec![FileLayer {
            disable: true,
            env_filter: "app_config=infoo".to_string(),
            ..Default::default()
        }];
        let err = init(&cfg).err().unwrap();
        assert!(err.is(BaseLogErr::NoOutput));
        assert!(!INSTALLED.load(std::sync::atomic::Ordering::Acquire));
    }

    /// 测试结束时把工作目录还原，即使 panic 也不留下副作用（这个进程接下来的测试还要用相对路径）
    #[test]
    #[serial_test::serial(log_init)]
    fn test_init_root_dir_empty_uses_default_dir() {
        // root_dir 为空时按 DEFAULT_ROOT_DIR（"./logs"，相对当前目录）建目录。用两个前缀重叠的 layer
        // 制造一个必然失败的 FileNameOverlap，从报错信息里的目录能看出确实用了默认目录。
        // 不切换当前目录（lib 测试多线程并行，改 cwd 会影响别的测试）：init 失败时会删掉这次新建的目录，
        // 所以不会在 crate 目录里留下 logs/
        let default_dir = Path::new(TracingConfig::DEFAULT_ROOT_DIR);
        let existed_before = default_dir.exists();

        let cfg = TracingConfig {
            root_dir: String::new(),
            layers: vec![layer("", "app"), layer("", "app-error")],
            ..Default::default()
        };
        let err = init(&cfg).err().unwrap();
        assert!(err.is(BaseLogErr::FileNameOverlap));

        let expect_dir = std::fs::canonicalize(std::env::current_dir().unwrap())
            .unwrap()
            .join(TracingConfig::DEFAULT_ROOT_DIR.trim_start_matches("./"))
            .join("app");
        assert!(
            err.to_string().contains(&expect_dir.display().to_string()),
            "{err}"
        );
        // 这次新建的默认目录，失败后清理掉了（原本就有的不动）
        assert_eq!(default_dir.exists(), existed_before);
        assert!(!INSTALLED.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn test_layer_name_format() {
        let dir = Path::new("/var/log/app");
        assert_eq!(
            layer_name(2, dir, &layer("ignored", "app-error")),
            "#2(/var/log/app/app-error*log)"
        );
        // 前缀为空：`Path::join("")` 会带一个尾部斜杠
        assert_eq!(
            layer_name(0, dir, &layer("ignored", "")),
            "#0(/var/log/app/*log)"
        );
        // 后缀为空：星号后面没有内容
        let mut no_suffix = layer("ignored", "app");
        no_suffix.filename_suffix = String::new();
        assert_eq!(layer_name(1, dir, &no_suffix), "#1(/var/log/app/app*)");
    }

    #[test]
    #[serial_test::serial(log_init)]
    fn test_init_appender_failed() {
        // 日志目录的上一级是个普通文件，目录建不出来
        let file = tempfile::NamedTempFile::new().unwrap();
        let dir = file.path().join("logs");
        let err = init(&file_layer(dir.to_str().unwrap())).err().unwrap();
        assert!(err.is(BaseLogErr::AppenderFailed));
        // 原始 io 错误挂在 source 上
        assert!(std::error::Error::source(&err).is_some());
        assert!(!INSTALLED.load(std::sync::atomic::Ordering::Acquire));
    }
}
