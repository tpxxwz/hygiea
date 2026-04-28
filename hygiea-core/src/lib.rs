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
fn init_hygiea_core() {
    error::init();
}

// ========== Feature: app ==========
#[cfg(feature = "app")]
pub mod app;

#[cfg(feature = "app")]
pub use app::{Component, Registry};
