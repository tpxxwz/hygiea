//! Application framework module
//!
//! Provides a component-based application framework with lifecycle management.

pub use anyhow::Error;
pub use async_trait::async_trait;
pub use log;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Component trait - defines the interface for application components
///
/// The lifetime parameter 'a represents the lifetime of the configuration
/// that components may hold references to.
#[async_trait::async_trait]
pub trait Component: Send {
    /// Startup the component
    ///
    /// Do all initialization that might fail here (e.g., bind port, connect to DB).
    /// If successful, spawn background task and return the JoinHandle.
    /// Return None if no background task is needed.
    ///
    /// The shutdown_rx parameter receives the shutdown signal from the registry.
    /// Components should monitor this channel and gracefully shutdown when a signal is received.
    ///
    /// Returns an error if startup fails - this will trigger shutdown of all previously started components.
    async fn startup(
        &mut self,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<Option<JoinHandle<()>>, Error>;
}

/// Component registry - manages component lifecycle
///
/// The lifetime parameter 'a represents the lifetime of component references.
/// Components may hold references to external data (e.g., configuration),
/// and those references must outlive the registry.
pub struct Registry<'a> {
    components: Vec<Box<dyn Component + 'a>>,
    handles: Vec<JoinHandle<()>>,
    shutdown_tx: broadcast::Sender<()>,
}

impl<'a> Registry<'a> {
    /// Create a new registry with configuration and register components
    ///
    /// The registration function receives:
    /// - `config`: Reference to the configuration
    /// - `components`: Mutable vector to push components into
    ///
    /// Components can hold references to the configuration.
    ///
    /// # Example
    /// ```ignore
    /// let config = load_config();
    /// Registry::new(&config, |config, components| {
    ///     components.push(Box::new(DatabaseComponent::new(config)));
    ///     components.push(Box::new(HttpServerComponent::new(config)));
    /// })
    /// .run()
    /// .await;
    /// ```
    pub fn new<C, F>(config: &'a C, register_fn: F) -> Self
    where
        F: FnOnce(&'a C, &mut Vec<Box<dyn Component + 'a>>),
    {
        let (shutdown_tx, _) = broadcast::channel(1);
        let mut components = Vec::new();

        register_fn(config, &mut components);

        Self {
            components,
            handles: Vec::new(),
            shutdown_tx,
        }
    }

    /// Run the application: startup all components, wait for shutdown, then cleanup
    ///
    /// This method:
    /// 1. Starts all components in registration order
    /// 2. Waits for shutdown signal (Ctrl+C or SIGTERM)
    /// 3. Gracefully shuts down all components in reverse order
    ///
    /// If any component fails to start:
    /// - All previously started components are gracefully shut down
    /// - Process exits with error code 1
    ///
    /// # Example
    /// ```ignore
    /// Registry::new(config, register_components)
    ///     .run()
    ///     .await;
    /// ```
    pub async fn run(mut self) {
        // Startup all components
        let mut started_count = 0;

        for (idx, c) in self.components.iter_mut().enumerate() {
            let shutdown_rx = self.shutdown_tx.subscribe();

            // Startup component
            match c.startup(shutdown_rx).await {
                Ok(Some(handle)) => {
                    log::info!("Component {} started with background task", idx);
                    self.handles.push(handle);
                    started_count += 1;
                }
                Ok(None) => {
                    log::info!("Component {} started (no background task)", idx);
                    started_count += 1;
                }
                Err(e) => {
                    self.shutdown_all().await;
                    log::error!("Component {} failed to start: {}", idx, e);
                    std::process::exit(1);
                }
            }
        }

        log::info!("All {} components started successfully", started_count);

        // Wait for shutdown signal
        wait_for_signal().await;

        // Trigger graceful shutdown
        log::info!("Starting graceful shutdown...");
        self.shutdown_all().await;
    }

    /// Shutdown all components by broadcasting shutdown signal
    /// Then wait for all background tasks to complete (in reverse order)
    async fn shutdown_all(&mut self) {
        // Send shutdown signal to all components
        let _ = self.shutdown_tx.send(());

        // Wait for all handles to complete (graceful shutdown)
        // Shutdown in reverse registration order
        for handle in self.handles.drain(..).rev() {
            if let Err(e) = handle.await {
                let msg = if e.is_panic() {
                    "Task panicked"
                } else {
                    "Task cancelled"
                };
                log::warn!("{}: {:?}", msg, e);
            }
        }
    }
}

/// Wait for Ctrl+C or SIGTERM
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                log::info!("Received Ctrl+C");
            }
            _ = unix_signal_shutdown() => {
                log::info!("Received SIGTERM");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
        log::info!("Received Ctrl+C");
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
