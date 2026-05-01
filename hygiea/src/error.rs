//! Error handling module

pub use hygiea_core::{FmtErr, RawErr};

pub use hygiea_macros::{fmt_err, raw_err};

#[doc(hidden)]
pub mod __private {
    pub use hygiea_core::{ERR_REGISTRATIONS, ErrRegistration, ErrRegistrationKind};
}

/// Success response marker
#[derive(raw_err)]
#[err_code_prefix = "000"]
pub enum SuccessRawErr {
    /// Success
    #[error(err_code = "00000", err_msg = "")]
    Success,
}

/// Base system-level raw errors
#[derive(raw_err)]
#[err_code_prefix = "999"]
pub enum BaseRawErr {
    /// Generic system error
    #[error(err_code = "09999", err_msg = "System Error")]
    SysRawErr,
}

/// Base formatted error types for system-level errors with context
#[derive(fmt_err)]
#[err_code_prefix = "999"]
pub enum BaseFmtErr {
    /// Generic system error with cause
    #[error(err_code = "99999", err_tpl = "System Error, cause: {{ cause }}")]
    SysFmtErr,
}
