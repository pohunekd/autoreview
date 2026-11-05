//! The review engine of autoreview: discovery, reviewer runs and publication.

// Rust guideline compliant 2026-06-26

pub mod config;
pub mod error;
pub mod integrations;
pub mod model;
pub mod publish;

#[cfg(test)]
mod testing;

#[doc(inline)]
pub use error::{BoxError, Error, WORKFLOW_FAILED, error_code, pohunek_error_code};
