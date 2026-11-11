//! Side effects of the review engine: the `gh` and pohunek CLIs and Matrix.

// Rust guideline compliant 2026-06-26

mod github;
mod integrations;
mod notify;
mod pohunek;
mod process;

pub use integrations::External;
pub use pohunek::local_pohunek_json;
