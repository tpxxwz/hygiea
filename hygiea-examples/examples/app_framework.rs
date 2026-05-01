//! Application framework example
//!
//! This example demonstrates:
//! - Named components: two DatabaseComponent instances (primary + replica)
//! - Single Component trait covering both build and startup
//! - Component dependencies via Resources (Redis depends on primary DB)
//! - Two-phase startup: access resources after startup for business logic
//! - Graceful shutdown
//!
//! Run with:
//! ```bash
//! cargo run -p hygiea-examples --example app_framework --features app
//! ```

use std::sync::{Arc, OnceLock};

use hygiea::{Component, Registry, Resources, async_trait};
use tokio::time::{Duration, interval};
use tracing;

// ========== Shared Resources (stored in Resources) ==========

#[derive(Debug)]
pub struct DbPool {
    pub url: String,
}

#[derive(Debug)]
pub struct RedisClient {
    pub url: String,
}

// ========== Configs ==========

#[derive(Debug)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug)]
pub struct RedisConfig {
    pub url: String,
}

#[derive(Debug)]
pub struct HttpConfig {
    pub port: u16,
}

// ========== Components ==========

pub struct DatabaseComponent {
    name: &'static str,
    config: DatabaseConfig,
}

#[async_trait]
impl Component for DatabaseComponent {
    type Config = DatabaseConfig;

    fn build(name: &'static str, config: Self::Config) -> Self {
        Self { name, config }
    }

    async fn startup(
        &mut self,
        state: &Resources,
        _shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, anyhow::Error> {
        tracing::info!("[{}] connecting to: {}", self.name, self.config.url);
        tokio::time::sleep(Duration::from_millis(100)).await;

        state.insert_named(
            self.name,
            DbPool {
                url: self.config.url.clone(),
            },
        );

        tracing::info!("[{}] DbPool registered", self.name);
        Ok(None)
    }
}

pub struct RedisComponent {
    name: &'static str,
    config: RedisConfig,
}

#[async_trait]
impl Component for RedisComponent {
    type Config = RedisConfig;

    fn build(name: &'static str, config: Self::Config) -> Self {
        Self { name, config }
    }

    async fn startup(
        &mut self,
        state: &Resources,
        _shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, anyhow::Error> {
        let primary = state
            .get_named::<DbPool>("primary")
            .expect("primary DbPool not found - DatabaseComponent(primary) must start first");

        tracing::info!(
            "[{}] connecting to: {} (primary DB: {})",
            self.name,
            self.config.url,
            primary.url
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        state.insert_named(
            self.name,
            RedisClient {
                url: self.config.url.clone(),
            },
        );

        tracing::info!("[{}] RedisClient registered", self.name);
        Ok(None)
    }
}

pub struct HttpServerComponent {
    config: HttpConfig,
}

#[async_trait]
impl Component for HttpServerComponent {
    type Config = HttpConfig;

    fn build(_name: &'static str, config: Self::Config) -> Self {
        Self { config }
    }

    async fn startup(
        &mut self,
        state: &Resources,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> Result<Option<tokio::task::JoinHandle<()>>, anyhow::Error> {
        let primary = state
            .get_named::<DbPool>("primary")
            .expect("primary DbPool not found");
        let replica = state
            .get_named::<DbPool>("replica")
            .expect("replica DbPool not found");
        let redis = state
            .get_named::<RedisClient>("cache")
            .expect("cache RedisClient not found");

        let port = self.config.port;
        tracing::info!(
            "HTTP server on port {}, primary={}, replica={}, redis={}",
            port,
            primary.url,
            replica.url,
            redis.url
        );

        Ok(Some(tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(2));
            let mut request_count = 0u32;

            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        tracing::info!("HTTP server shutting down ({} requests served)", request_count);
                        break;
                    }
                    _ = ticker.tick() => {
                        request_count += 1;
                        tracing::info!("Request #{}", request_count);
                    }
                }
            }
        })))
    }
}

// ========== Application State ==========

#[derive(Clone, Debug)]
pub struct AppState {
    pub primary_db: Arc<DbPool>,
    pub replica_db: Arc<DbPool>,
    pub redis: Arc<RedisClient>,
}

static APP_STATE: OnceLock<AppState> = OnceLock::new();

impl AppState {
    pub fn init(state: AppState) {
        APP_STATE.set(state).expect("AppState already initialized");
    }

    fn global() -> &'static Self {
        APP_STATE.get().expect("AppState not initialized")
    }

    pub fn primary_db() -> &'static Arc<DbPool> {
        &Self::global().primary_db
    }
    pub fn replica_db() -> &'static Arc<DbPool> {
        &Self::global().replica_db
    }
    pub fn redis() -> &'static Arc<RedisClient> {
        &Self::global().redis
    }
}

// ========== Main ==========

#[tokio::main]
async fn main() {
    println!("╔════════════════════════════════════════════╗");
    println!("║  Hygiea Application Framework Example     ║");
    println!("╚════════════════════════════════════════════╝\n");

    let mut registry = Registry::new()
        .add_named::<DatabaseComponent>(
            "primary",
            DatabaseConfig {
                url: "postgres://primary-host/mydb".to_string(),
            },
        )
        .add_named::<DatabaseComponent>(
            "replica",
            DatabaseConfig {
                url: "postgres://replica-host/mydb".to_string(),
            },
        )
        .add_named::<RedisComponent>(
            "cache",
            RedisConfig {
                url: "redis://localhost:6379".to_string(),
            },
        )
        .add::<HttpServerComponent>(HttpConfig { port: 8080 });

    println!("━━━━ Starting components ━━━━\n");
    if let Err(e) = registry.launch().await {
        tracing::error!("{}", e);
        std::process::exit(1);
    }

    let resources = registry.resources();
    AppState::init(AppState {
        primary_db: resources.get_named::<DbPool>("primary").unwrap(),
        replica_db: resources.get_named::<DbPool>("replica").unwrap(),
        redis: resources.get_named::<RedisClient>("cache").unwrap(),
    });

    println!("\n━━━━ AppState (global) ━━━━");
    println!("  primary DB : {}", AppState::primary_db().url);
    println!("  replica  DB: {}", AppState::replica_db().url);
    println!("  Redis      : {}", AppState::redis().url);
    println!("\n  Press Ctrl+C to shutdown");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    registry.await_shutdown().await;

    println!("\nShutdown complete! Goodbye!");
}
