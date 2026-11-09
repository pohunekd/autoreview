//! The MCP tools: their schemas, the scope check, the engine call and the log line.

// Rust guideline compliant 2026-06-26

use std::sync::Arc;

use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::access::Scope;
use super::results::{envelope, queue_view};
use crate::config::identifier_valid;
use crate::engine::{Clock, Engine};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::model::repository_valid;
use crate::store::{
    REVIEW_AWAITING, REVIEW_COMPLETED, REVIEW_QUEUED, REVIEW_RUNNING, ReviewFilter,
};

/// Name of the server in the `initialize` result.
const SERVER_NAME: &str = "autoreview";

/// Error code of tool arguments that do not match the tool's schema or rules.
const COMMAND_INVALID: &str = "command_invalid";

/// Error code of a call to a tool that the server does not have.
const TOOL_UNKNOWN: &str = "mcp_tool_unknown";

/// Error code of a write tool called by a client without the write scope.
const SCOPE_DENIED: &str = "mcp_scope_denied";

/// Error code of a call that reached a tool without an authenticated client.
const CLIENT_MISSING: &str = "mcp_client_missing";

/// Error code of a result that cannot be encoded as JSON.
const RESULT_INVALID: &str = "mcp_result_invalid";

/// The review statuses that `list_reviews` filters by.
const STATUSES: [&str; 4] = [
    REVIEW_QUEUED,
    REVIEW_RUNNING,
    REVIEW_AWAITING,
    REVIEW_COMPLETED,
];

/// The authenticated client of one request, placed in the request extensions by the service.
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    /// Name of the client in the configuration.
    pub(crate) name: String,
    /// What the client may do.
    pub(crate) scope: Scope,
}

/// One tool: its name, the scope it needs and its description.
struct ToolSpec {
    /// Name of the tool.
    name: &'static str,
    /// Scope a client needs to call it.
    scope: Scope,
    /// Description shown to the calling agent.
    description: &'static str,
}

/// Every tool of the server, read tools first.
const TOOLS: [ToolSpec; 5] = [
    ToolSpec {
        name: "list_reviews",
        scope: Scope::Read,
        description: "Lists the newest reviews, optionally of one status and one repository.",
    },
    ToolSpec {
        name: "review_detail",
        scope: Scope::Read,
        description: "Shows one review by workflow ID: findings, decision and publication URL.",
    },
    ToolSpec {
        name: "list_awaiting",
        scope: Scope::Read,
        description: "Lists the reviews that wait for a human approval.",
    },
    ToolSpec {
        name: "pull_request_status",
        scope: Scope::Read,
        description: "Shows everything the engine knows about one pull request.",
    },
    ToolSpec {
        name: "review_queue",
        scope: Scope::Read,
        description: "Lists the requested reviews with their engine status; sends nothing.",
    },
];

/// Arguments of `list_reviews`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArguments {
    /// Status the reviews have.
    status: Option<String>,
    /// Repository the reviews are of.
    repository: Option<String>,
    /// Most reviews returned, from 1 to `cli.review_list_limit`.
    limit: Option<i64>,
}

/// Arguments of `review_detail`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DetailArguments {
    /// Workflow ID of the review.
    workflow_id: String,
}

/// Arguments of `pull_request_status`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PullRequestArguments {
    /// Repository as `owner/name`.
    repository: String,
    /// Pull request number.
    number: i64,
}

/// Arguments of the tools that take none.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArguments {}
