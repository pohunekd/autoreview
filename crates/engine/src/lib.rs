//! The review engine of autoreview: discovery, reviewer runs and publication.

// Rust guideline compliant 2026-06-26

pub mod config;
pub mod credential_check;
pub(crate) mod discovery;
pub mod discussion;
pub(crate) mod engine;
pub mod error;
pub mod integrations;
pub mod mcp;
pub(crate) mod messages;
pub mod metrics;
pub mod model;
pub(crate) mod prior;
pub(crate) mod protocol;
pub mod publish;
pub(crate) mod review;
pub(crate) mod review_queue;
pub mod stats;
pub mod store;
pub(crate) mod verify;

#[cfg(test)]
mod testing;

#[doc(inline)]
pub use discovery::commands::PullRequestReviewStatus;
#[doc(inline)]
pub use engine::{Clock, Engine, SystemClock};
#[doc(inline)]
pub use error::{BoxError, Error, WORKFLOW_FAILED, error_code, pohunek_error_code};
#[doc(inline)]
pub use protocol::request::go_json;
#[doc(inline)]
pub use review_queue::QueueResult;
