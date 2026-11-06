//! The review engine of autoreview: discovery, reviewer runs and publication.

// Rust guideline compliant 2026-06-26

pub mod config;
pub mod discussion;
pub(crate) mod engine;
pub mod error;
pub mod integrations;
pub(crate) mod messages;
pub mod model;
pub(crate) mod prior;
pub(crate) mod protocol;
pub mod publish;
pub(crate) mod review_queue;
pub mod store;
pub(crate) mod verify;

#[cfg(test)]
mod testing;

#[doc(inline)]
pub use error::{BoxError, Error, WORKFLOW_FAILED, error_code, pohunek_error_code};
