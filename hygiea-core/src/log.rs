//! 日志：tracing 的配置和初始化，不依赖组件框架。
//!
//! 组件框架里的接入（作为第一个组件启动）在 `app` 模块的 `TracingComponent`。

use serde::Deserialize;
use tracing_appender::non_blocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_log::LogTracer;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt};

use anyhow::Context;

use crate::datetime::{DateTimeFormatter, now_local};

// ---- config types ----------------------------------------------------------

#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct ConsoleLayer {
    pub disable: bool,
    pub env_filter: String,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct FileLayer {
    pub disable: bool,
    pub env_filter: String,
    pub dir: String,
    pub filename_prefix: String,
    pub filename_suffix: String,
    pub non_blocking: bool,
    pub rolling: String,
    pub max_log_files: usize,
}

impl Default for FileLayer {
    fn default() -> Self {
        Self {
            disable: false,
            env_filter: String::new(),
            dir: "./logs".to_string(),
            filename_prefix: "app".to_string(),
            filename_suffix: "log".to_string(),
            non_blocking: true,
            rolling: "Never".to_string(),
            max_log_files: 1,
        }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct TracingConfig {
    pub console: ConsoleLayer,
    pub layers: Vec<FileLayer>,
    pub root_env_filter: String,
    pub root_dir: String,
    pub time_format: Option<DateTimeFormatter>,
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
            time_format: Some(DateTimeFormatter::default()),
        }
    }
}

// ---- init ------------------------------------------------------------------

/// 持有非阻塞文件写入的 guard，drop 时把缓冲里的日志刷出去，所以要一直持有到进程结束
pub struct LogGuard(#[allow(dead_code)] Vec<WorkerGuard>);

/// 按配置安装全局 subscriber，并把 `log` crate 的日志桥接进来。一个进程只能成功调用一次。
///
/// 用 [`crate::app::Registry`] 时不用自己调，它会作为第一个组件按配置初始化；
/// 不用组件框架的程序（脚本、CLI）直接调这个，返回的 guard 要一直持有
pub fn init(cfg: &TracingConfig) -> anyhow::Result<LogGuard> {
    init_tracing(cfg).map(LogGuard)
}

/// 用默认配置初始化：只输出到控制台，级别 `info`，
/// 本地时间。脚本、小工具里一行搞定：`let _guard = hygiea::log::init_default()?;`
pub fn init_default() -> anyhow::Result<LogGuard> {
    init(&TracingConfig::default())
}

#[derive(Clone)]
struct LocalTime {
    format: Option<DateTimeFormatter>,
}

impl FormatTime for LocalTime {
    fn format_time(&self, writer: &mut Writer<'_>) -> std::fmt::Result {
        let now = now_local();
        let timestamp = match self.format {
            Some(formatter) => formatter.format(&now),
            None => DateTimeFormatter::default().format(&now),
        }
        .map_err(|_| std::fmt::Error)?;
        writer.write_str(&timestamp)
    }
}

fn init_tracing(cfg: &TracingConfig) -> anyhow::Result<Vec<WorkerGuard>> {
    let timer = LocalTime {
        format: cfg.time_format,
    };

    let mut work_guards: Vec<WorkerGuard> = Vec::new();

    let effective_root_filter = if cfg.root_env_filter.is_empty() {
        TracingConfig::DEFAULT_ROOT_ENV_FILTER
    } else {
        &cfg.root_env_filter
    };

    let mut combined_layer: Option<Box<dyn Layer<Registry> + Send + Sync>> = None;

    // Console layer
    if !cfg.console.disable {
        let filter_str = if cfg.console.env_filter.is_empty() {
            effective_root_filter
        } else {
            &cfg.console.env_filter
        };
        let console_layer = fmt::layer()
            .with_timer(timer.clone())
            .with_target(true)
            .with_filter(EnvFilter::new(filter_str));
        combined_layer = Some(Box::new(console_layer));
    }

    // File layers
    for layer_cfg in &cfg.layers {
        if layer_cfg.disable {
            continue;
        }
        let rotation = match layer_cfg.rolling.as_str() {
            "Minutely" => Rotation::MINUTELY,
            "Hourly" => Rotation::HOURLY,
            "Daily" => Rotation::DAILY,
            "Weekly" => Rotation::WEEKLY,
            "Never" => Rotation::NEVER,
            other => {
                return Err(anyhow::anyhow!(
                    "Invalid rolling value '{}'. Expected: Minutely, Hourly, Daily, Weekly, Never",
                    other
                ));
            }
        };

        let appender = RollingFileAppender::builder()
            .rotation(rotation)
            .filename_prefix(&layer_cfg.filename_prefix)
            .filename_suffix(&layer_cfg.filename_suffix)
            .max_log_files(layer_cfg.max_log_files)
            .build(&layer_cfg.dir)
            .context("Failed to create RollingFileAppender")?;

        let filter_str = if layer_cfg.env_filter.is_empty() {
            effective_root_filter
        } else {
            &layer_cfg.env_filter
        };
        let file_filter = EnvFilter::new(filter_str);

        if layer_cfg.non_blocking {
            let (writer, guard) = non_blocking(appender);
            work_guards.push(guard);
            let file_layer = fmt::layer()
                .with_timer(timer.clone())
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(file_filter);
            combined_layer = Some(match combined_layer {
                Some(existing) => Box::new(existing.and_then(file_layer)),
                None => Box::new(file_layer),
            });
        } else {
            let file_layer = fmt::layer()
                .with_timer(timer.clone())
                .with_ansi(false)
                .with_writer(appender)
                .with_filter(file_filter);
            combined_layer = Some(match combined_layer {
                Some(existing) => Box::new(existing.and_then(file_layer)),
                None => Box::new(file_layer),
            });
        };
    }

    // Fallback default console layer
    let combined_layer = match combined_layer {
        Some(layer) => layer,
        None => {
            let default_filter = EnvFilter::new(effective_root_filter);
            Box::new(
                fmt::layer()
                    .with_timer(timer.clone())
                    .with_target(true)
                    .with_filter(default_filter),
            )
        }
    };

    LogTracer::init().context("Failed to init LogTracer")?;

    let subscriber = Registry::default().with(combined_layer);
    tracing::subscriber::set_global_default(subscriber)
        .context("Set global default subscriber failed")?;
    Ok(work_guards)
}
