//! Tracing component for the application framework.

use serde::Deserialize;
use tracing_appender::non_blocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_log::LogTracer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt};

use crate::app::{Component, Resources, async_trait};


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
    pub time_format: Option<String>,
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
            time_format: None,
        }
    }
}

// ---- component -------------------------------------------------------------

pub struct TracingComponent {
    config: TracingConfig,
    guards: Vec<WorkerGuard>,
}

impl TracingComponent {
    pub fn new(config: TracingConfig) -> Self {
        Self {
            config,
            guards: Vec::new(),
        }
    }
}

#[async_trait]
impl Component for TracingComponent {
    type Config = TracingConfig;

    fn build(_name: &'static str, config: Self::Config) -> Self {
        Self {
            config,
            guards: Vec::new(),
        }
    }

    async fn startup(
        &mut self,
        _resources: &Resources,
        _shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, anyhow::Error> {
        self.guards = init_tracing(&self.config)?;
        Ok(None)
    }
}

// ---- init ------------------------------------------------------------------

fn init_tracing(cfg: &TracingConfig) -> anyhow::Result<Vec<WorkerGuard>> {
    use crate::date::DateFormat;
    let fmt = if let Some(s) = cfg.time_format.as_deref().filter(|s| !s.is_empty()) {
        time::format_description::parse_owned::<2>(s)
            .map_err(|e| anyhow::anyhow!("Invalid time_format: {e}"))?
    } else {
        let items = crate::date::Formatter::ISO.pattern_time();
        time::format_description::OwnedFormatItem::Compound(
            items.iter().map(|i| i.clone().into()).collect(),
        )
    };
    let timer = tracing_subscriber::fmt::time::UtcTime::new(fmt);

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
            .map_err(|e| anyhow::anyhow!("Failed to create RollingFileAppender: {}", e))?;

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
            Box::new(fmt::layer().with_timer(timer.clone()).with_target(true).with_filter(default_filter))
        }
    };

    LogTracer::init().map_err(|e| anyhow::anyhow!("Failed to init LogTracer: {}", e))?;

    let subscriber = Registry::default().with(combined_layer);
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| anyhow::anyhow!("Set global default subscriber failed: {}", e))?;
    Ok(work_guards)
}
