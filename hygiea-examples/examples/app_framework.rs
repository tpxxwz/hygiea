//! Application framework example
//!
//! This example demonstrates how to use the hygiea application framework
//! with generic configuration and component registration.
//!
//! ## Key Features
//! - Generic configuration support
//! - Flexible component registration via function callback
//! - Graceful shutdown handling
//!
//! Run with:
//! ```bash
//! cargo run --example app_framework --features app
//! ```

use hygiea::{Component, Registry, async_trait};
use tokio::time::{Duration, interval};

// ========== Generic Configuration Trait ==========

/// Trait for application configuration
pub trait AppConfig: Clone + Send + Sync + 'static {
    /// Get database connection string
    fn db_url(&self) -> &str;

    /// Get HTTP server port
    fn http_port(&self) -> u16;
}

// ========== Example Configuration ==========

#[derive(Clone, Debug)]
pub struct MyConfig {
    pub db_url: String,
    pub http_port: u16,
}

impl AppConfig for MyConfig {
    fn db_url(&self) -> &str {
        &self.db_url
    }

    fn http_port(&self) -> u16 {
        self.http_port
    }
}

// ========== Example Components ==========

pub struct DatabaseComponent<'a> {
    db_url: &'a str,
}

impl<'a> DatabaseComponent<'a> {
    pub fn new<C>(config: &'a C) -> Self
    where
        C: AppConfig,
    {
        Self {
            db_url: config.db_url(),
        }
    }
}

#[async_trait]
impl Component for DatabaseComponent<'_> {
    async fn startup(
        &mut self,
        _shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, hygiea::Error> {
        log::info!("📦 Database connecting to: {}", self.db_url);
        // Simulate database connection (synchronous, no background task needed)
        tokio::time::sleep(Duration::from_millis(100)).await;
        log::info!("✅ Database connected");
        Ok(None) // No background task for this component
    }
}

pub struct HttpServerComponent {
    port: u16,
}

impl HttpServerComponent {
    pub fn new<C>(config: &C) -> Self
    where
        C: AppConfig,
    {
        Self {
            port: config.http_port(),
        }
    }
}

#[async_trait]
impl Component for HttpServerComponent {
    async fn startup(
        &mut self,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, hygiea::Error> {
        let port = self.port;

        log::info!("🌐 HTTP server listening on port {}", port);

        // Spawn background task with graceful shutdown support
        Ok(Some(tokio::spawn(async move {
            log::info!("✅ HTTP server task running");

            let mut ticker = interval(Duration::from_secs(2));
            let mut request_count = 0;

            // Server loop - monitor both shutdown signal and work
            loop {
                tokio::select! {
                    // Check for shutdown signal
                    _ = shutdown_rx.recv() => {
                        log::info!("⏳ HTTP server received shutdown signal");
                        log::info!("🔄 Cleaning up... (processed {} requests)", request_count);
                        break;
                    }
                    // Simulate handling requests
                    _ = ticker.tick() => {
                        request_count += 1;
                        log::info!("📨 Processing request #{} on port {}", request_count, port);
                    }
                }
            }

            log::info!("🛑 HTTP server task stopped gracefully");
        })))
    }
}

pub struct HealthCheckComponent;

#[async_trait]
impl Component for HealthCheckComponent {
    async fn startup(
        &mut self,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, hygiea::Error> {
        log::info!("💚 Health check service starting");

        Ok(Some(tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(5));

            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        log::info!("💚 Health check service shutting down");
                        break;
                    }
                    _ = ticker.tick() => {
                        log::info!("💚 All systems operational");
                    }
                }
            }
        })))
    }
}

// ========== Component Registration Function ==========

/// Register all application components
///
/// This function is called by the framework with the config and components vector.
/// Users implement this to define which components to instantiate.
pub fn register_components<'a, C>(config: &'a C, components: &mut Vec<Box<dyn Component + 'a>>)
where
    C: AppConfig,
{
    components.push(Box::new(DatabaseComponent::new(config)));
    components.push(Box::new(HttpServerComponent::new(config)));
    components.push(Box::new(HealthCheckComponent));
}

// ========== Main ==========

#[tokio::main]
async fn main() {
    // Initialize logger for example
    env_logger::init();

    println!("╔════════════════════════════════════════════╗");
    println!("║  Hygiea Application Framework Example     ║");
    println!("╚════════════════════════════════════════════╝\n");

    // Create configuration
    let config = MyConfig {
        db_url: "postgres://localhost/mydb".to_string(),
        http_port: 8080,
    };

    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Starting application...");
    println!("  Press Ctrl+C to shutdown");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    // Method 1: Use a named function (Recommended)
    Registry::new(&config, register_components).run().await;

    // Method 2: Use a closure (Alternative)
    // Registry::new(&config, |config, components| {
    //     components.push(Box::new(DatabaseComponent::new(config)));
    //     components.push(Box::new(HttpServerComponent::new(config)));
    //     components.push(Box::new(HealthCheckComponent));
    // })
    // .run()
    // .await;

    println!("\n✅ Shutdown complete! Goodbye!");
}
