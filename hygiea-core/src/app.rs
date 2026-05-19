//! Application framework module
//!
//! Provides a component-based application framework with lifecycle management.

// ---- imports: std → external crates ----------------------------------------
use std::any::{Any, TypeId};
use std::sync::Arc;

use clap::Parser;
use dashmap::DashMap;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::log::{TracingComponent, TracingConfig};

// ---- re-exports & type aliases --------------------------------------------
pub use async_trait::async_trait;

// ---- private helper types --------------------------------------------------

#[derive(clap::Parser)]
#[command(author, version, about)]
struct AppCli {
    #[arg(long, default_value = "dev")]
    env: String,
    #[arg(short = 'f', long, value_delimiter = ',')]
    configs: Vec<String>,
}

#[derive(Hash, PartialEq, Eq)]
struct ComponentKey {
    type_id: TypeId,
    name: &'static str,
}

// ---- public types ----------------------------------------------------------

/// Top-level configuration for the `Registry`.
///
/// Each field is optional — framework-level components are only registered
/// when their config is `Some(...)`.
///
/// ```
/// use hygiea_core::app::RegistryConfig;
/// use hygiea_core::log::TracingConfig;
///
/// // default: console tracing only
/// let config = RegistryConfig::default();
///
/// // custom tracing
/// let config = RegistryConfig {
///     tracing: TracingConfig { ..Default::default() },
/// };
/// ```
#[derive(Deserialize, Clone, Default)]
pub struct RegistryConfig {
    #[serde(default)]
    pub tracing: TracingConfig,
}

/// Trait for config types that can produce a [`RegistryConfig`].
///
/// Implement this on your application's top-level config struct so that
/// [`Registry::from_file`] can initialize the registry in one call.
pub trait IntoRegistryConfig {
    fn registry_config(&self) -> RegistryConfig;
}

/// Error returned when a component fails to start
#[derive(Debug)]
pub struct LaunchError {
    pub index: usize,
    pub name: &'static str,
    pub type_name: &'static str,
    pub source: anyhow::Error,
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "component[{}] {}({}) failed to start: {}",
            self.index, self.type_name, self.name, self.source
        )
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

/// Shared resource container accessible by all components
///
/// Components can read and write typed resources during startup.
/// Uses type erasure internally, but provides a typed API.
///
/// Supports both anonymous and named resources of the same type,
/// so multiple instances of the same type (e.g. two PgPool) can coexist.
///
/// # Example
/// ```ignore
/// // Anonymous (single instance per type)
/// resources.insert(RedisClient::new(config));
/// let redis = resources.get::<RedisClient>().unwrap();
///
/// // Named (multiple instances of same type)
/// resources.insert_named("primary", PgPool::new(primary_config));
/// resources.insert_named("replica", PgPool::new(replica_config));
/// let primary = resources.get_named::<PgPool>("primary").unwrap();
/// let replica  = resources.get_named::<PgPool>("replica").unwrap();
/// ```
pub struct Resources {
    map: Arc<DashMap<ComponentKey, Arc<dyn Any + Send + Sync>>>,
}

impl Resources {
    fn new() -> Self {
        Self {
            map: Arc::new(DashMap::new()),
        }
    }

    fn key<T: 'static>(name: &'static str) -> ComponentKey {
        ComponentKey {
            type_id: TypeId::of::<T>(),
            name,
        }
    }

    /// Insert an anonymous typed resource
    pub fn insert<T: Send + Sync + 'static>(&self, value: T) {
        self.insert_named("", value);
    }

    /// Insert a named typed resource
    pub fn insert_named<T: Send + Sync + 'static>(&self, name: &'static str, value: T) {
        self.map.insert(Self::key::<T>(name), Arc::new(value));
    }

    /// Get an anonymous typed resource
    pub fn get<T: Send + Sync + Clone + 'static>(&self) -> Option<T> {
        self.get_named("")
    }

    /// Get a named typed resource
    pub fn get_named<T: Send + Sync + Clone + 'static>(&self, name: &'static str) -> Option<T> {
        self.map
            .get(&Self::key::<T>(name))
            .and_then(|entry| (**entry).downcast_ref::<T>().map(T::clone))
    }

    /// Check if an anonymous resource type exists
    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.contains_named::<T>("")
    }

    /// Check if a named resource type exists
    pub fn contains_named<T: Send + Sync + 'static>(&self, name: &'static str) -> bool {
        self.map.contains_key(&Self::key::<T>(name))
    }

    /// Remove an anonymous typed resource
    pub fn remove<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.remove_named("")
    }

    /// Remove a named typed resource
    pub fn remove_named<T: Send + Sync + 'static>(&self, name: &'static str) -> Option<Arc<T>> {
        self.map
            .remove(&Self::key::<T>(name))
            .and_then(|(_, arc)| Arc::clone(&arc).downcast::<T>().ok())
    }
}

impl Clone for Resources {
    fn clone(&self) -> Self {
        Self {
            map: Arc::clone(&self.map),
        }
    }
}

// ---- public traits ---------------------------------------------------------

/// Component trait - defines the full lifecycle interface for application components
///
/// Implement this single trait to define both how a component is built and how it starts up.
/// `name` is the key used for named resource storage; use `""` for anonymous components.
#[async_trait]
pub trait Component: Send + 'static {
    /// Configuration type for this component
    type Config;

    /// Build the component from its name and config.
    ///
    /// Store `name` in the struct if you need it in `startup`
    /// (e.g. `state.insert_named(self.name, pool)`).
    fn build(name: &'static str, config: Self::Config) -> Self;

    /// Start the component.
    ///
    /// Do all initialization that might fail here (e.g., bind port, connect to DB).
    /// Read dependencies from `state` and insert your own resources for later components.
    /// Spawn a background task and return its handle if needed, or return `None`.
    /// On error, previously started components will be shut down automatically.
    async fn startup(
        &mut self,
        state: &Resources,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<Option<JoinHandle<()>>, anyhow::Error>;
}

/// Component registry - manages component lifecycle
pub struct Registry {
    state: Resources,
    components: Vec<(TypeId, &'static str, &'static str, Box<dyn ComponentObject>)>,
    handles: Vec<JoinHandle<()>>,
    shutdown_tx: broadcast::Sender<()>,
}

impl Registry {
    /// Create a new registry with default config (console tracing, level `info`).
    pub fn new() -> Self {
        Self::with_config(RegistryConfig::default())
    }

    fn with_config(config: RegistryConfig) -> Self {
        Self {
            state: Resources::new(),
            components: Vec::new(),
            handles: Vec::new(),
            shutdown_tx: broadcast::channel(1).0,
        }
        .add::<TracingComponent>(config.tracing)
    }

    /// Load config from file and initialize the registry in one call.
    ///
    /// Load config from file and initialize the registry in one call.
    ///
    /// Reads `--env <name>` (default `dev`) and optional `-f <file,...>` extra files.
    /// Config files are resolved as `config/<name>.toml` relative to the working directory.
    /// `T` must implement [`IntoRegistryConfig`] to provide the framework-level config.
    /// Returns `(registry, config)` so the caller can still access all config fields.
    pub fn load_config<T>() -> (Self, T)
    where
        T: serde::de::DeserializeOwned + IntoRegistryConfig,
    {
        let cli = AppCli::parse();
        println!("Environment: [{}]", cli.env);

        let primary = format!("config/{}.toml", cli.env);
        println!("Loading config: [{}]", primary);

        let mut builder = config::Config::builder().add_source(config::File::with_name(&primary));

        for extra in &cli.configs {
            let path = format!("config/{}.toml", extra.trim_end_matches(".toml"));
            println!("Loading extra config: [{}]", path);
            builder = builder.add_source(config::File::with_name(&path));
        }

        let config: T = builder
            .build()
            .unwrap_or_else(|e| panic!("Failed to load '{}': {}", primary, e))
            .try_deserialize()
            .unwrap_or_else(|e| panic!("Failed to parse '{}': {}", primary, e));

        let registry = Self::with_config(config.registry_config());
        (registry, config)
    }

    /// Add an anonymous component (single instance per type)
    pub fn add<C: Component>(self, config: C::Config) -> Self {
        self.add_named::<C>("", config)
    }

    /// Add a named component, allowing multiple instances of the same type.
    ///
    /// `name` is forwarded to `Component::build` so the component can store it
    /// and use it in `startup` (e.g. `state.insert_named(self.name, pool)`).
    pub fn add_named<C: Component>(mut self, name: &'static str, config: C::Config) -> Self {
        self.components.push((
            TypeId::of::<C>(),
            name,
            std::any::type_name::<C>(),
            Box::new(C::build(name, config)),
        ));
        self
    }

    /// Launch all components, build application state, then run until shutdown signal.
    pub async fn run<F>(mut self, f: F)
    where
        F: FnOnce(&Resources),
    {
        match self.start_components().await {
            Err(e) => tracing::error!("{}", e),
            Ok(()) => {
                f(&self.state);
                tracing::info!("Application ready");
                wait_for_signal().await;
                tracing::info!("Starting graceful shutdown...");
                self.shutdown().await;
                tracing::info!("Shutdown complete.");
            }
        }
    }

    async fn start_components(&mut self) -> Result<(), LaunchError> {
        for idx in 0..self.components.len() {
            let name = self.components[idx].1;
            let type_name = self.components[idx].2;
            let shutdown_rx = self.shutdown_tx.subscribe();
            let result = self.components[idx]
                .3
                .startup(&self.state, shutdown_rx)
                .await;

            match result {
                Ok(Some(handle)) => {
                    tracing::info!("{}({}) started with background task", type_name, name);
                    self.handles.push(handle);
                }
                Ok(None) => {
                    tracing::info!("{}({}) started", type_name, name);
                }
                Err(e) => {
                    let err = LaunchError {
                        index: idx,
                        name,
                        type_name,
                        source: e,
                    };
                    tracing::error!("{}", err);
                    self.shutdown().await;
                    return Err(err);
                }
            }
        }

        tracing::info!(
            "All {} components started successfully",
            self.components.len()
        );
        Ok(())
    }

    /// Shutdown all components by broadcasting shutdown signal
    /// Then wait for all background tasks to complete (in reverse order)
    async fn shutdown(&mut self) {
        let _ = self.shutdown_tx.send(());

        for handle in self.handles.drain(..).rev() {
            if let Err(e) = handle.await {
                let msg = if e.is_panic() {
                    "Task panicked"
                } else {
                    "Task cancelled"
                };
                tracing::warn!("{}: {:?}", msg, e);
            }
        }
    }
}

// ---- private traits & impls ------------------------------------------------

/// Internal object-safe trait for storing heterogeneous components in the Registry.
/// Only exposes `startup`; `build` and `Config` are not object-safe.
#[async_trait]
trait ComponentObject: Send + 'static {
    async fn startup(
        &mut self,
        state: &Resources,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<Option<JoinHandle<()>>, anyhow::Error>;
}

#[async_trait]
impl<C: Component> ComponentObject for C {
    async fn startup(
        &mut self,
        state: &Resources,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<Option<JoinHandle<()>>, anyhow::Error> {
        Component::startup(self, state, shutdown_rx).await
    }
}

// ---- free functions --------------------------------------------------------

/// Wait for Ctrl+C or SIGTERM
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received Ctrl+C");
            }
            _ = unix_signal_shutdown() => {
                tracing::info!("Received SIGTERM");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
        tracing::info!("Received Ctrl+C");
    }
}

/// Listen for SIGTERM signal (Unix/Linux/macOS only)
#[cfg(unix)]
async fn unix_signal_shutdown() {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to install SIGTERM handler")
        .recv()
        .await;
}
