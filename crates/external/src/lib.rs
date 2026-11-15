//! Side effects of the review engine: the `gh`, pohunek and git CLIs and the notifiers.

// Rust guideline compliant 2026-06-26

mod git;
mod github;
mod integrations;
mod notify;
mod pohunek;
mod process;

pub use git::{branch_refs, delete_ref, worktrees};
pub use integrations::External;
pub use pohunek::branches::{host_is_local, project_roots, session_branches};
pub use pohunek::local_pohunek_json;
