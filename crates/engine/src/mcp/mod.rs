//! The MCP server of autoreview: read tools for every client, write tools for write clients.
//!
//! The binary binds the listener; this module answers each HTTP request through the engine
//! functions the CLI commands call.

// Rust guideline compliant 2026-06-26

#[expect(dead_code, reason = "the MCP server of round 11 checks the tokens")]
pub(crate) mod access;

#[doc(inline)]
pub use access::{McpClient, McpClientEntry, McpConfig, Scope};
