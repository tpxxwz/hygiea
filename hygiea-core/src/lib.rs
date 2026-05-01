// ========== Feature: template ==========
#[cfg(feature = "template")]
mod template;

#[cfg(feature = "template")]
pub use template::{format_positional, format_template_cached, format_template_once};

// ========== Feature: error ==========
#[cfg(feature = "error")]
mod error;

#[cfg(feature = "error")]
pub use error::{ERR_REGISTRATIONS, ErrRegistration, ErrRegistrationKind, FmtErr, RawErr};

#[cfg(feature = "error")]
#[ctor::ctor]
fn init_hygiea_error_env() {
    error::init();
}

// ========== Feature: app ==========
#[cfg(feature = "app")]
pub mod app;

#[cfg(feature = "app")]
pub mod log;

#[cfg(feature = "app")]
pub use app::{Component, LaunchError, Registry, RegistryConfig, Resources};

#[cfg(feature = "app")]
pub use log::{ConsoleLayer, FileLayer, TracingConfig};

// ========== ext (env_tool always, distributed-lock optional) ==========
pub mod ext;
pub use ext::{BuiltinKey, EnvKey, env, env_get, env_get_opt, env_get_or, env_get_or_else};

#[cfg(feature = "distributed-lock")]
pub use ext::{DistributedKey, DistributedLock};
