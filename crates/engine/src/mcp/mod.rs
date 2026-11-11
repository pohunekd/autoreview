//! The MCP server of autoreview: read tools for every client, write tools for write clients.
//!
//! The binary binds the listener; this module answers each HTTP request through the engine
//! functions the CLI commands call.

// Rust guideline compliant 2026-06-26

pub(crate) mod access;
pub(crate) mod host;
pub(crate) mod results;
pub(crate) mod service;
pub(crate) mod tools;

#[doc(inline)]
pub use access::{McpClient, McpClientEntry, McpConfig, McpLimits, Scope};
#[doc(inline)]
pub use service::{MCP_PATH, McpServer, ResponseBody};
