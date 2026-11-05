//! The review engine of autoreview: discovery, reviewer runs and publication.

// Rust guideline compliant 2026-06-26

pub mod config;
pub mod error;
pub mod model;
pub mod publish;

#[doc(inline)]
pub use error::{BoxError, Error, WORKFLOW_FAILED, error_code, pohunek_error_code};
