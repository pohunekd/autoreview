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
use crate::model::{ApproveRequest, ResolveRequest, RetryRequest, repository_valid};
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
const TOOLS: [ToolSpec; 8] = [
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
    ToolSpec {
        name: "resolve",
        scope: Scope::Write,
        description: "Finds the review that waits for approval at a head or head prefix.",
    },
    ToolSpec {
        name: "approve",
        scope: Scope::Write,
        description: "Approves the clean review that waits at the full head; the engine \
                      publishes it.",
    },
    ToolSpec {
        name: "retry",
        scope: Scope::Write,
        description: "Starts a new attempt of a stopped review at its current head.",
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

/// Arguments of `resolve` and `retry`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PrefixArguments {
    /// Repository as `owner/name`.
    repository: String,
    /// Pull request number.
    number: i64,
    /// Head SHA or a prefix of at least 12 characters.
    head_prefix: String,
}

/// Arguments of `approve`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveArguments {
    /// Repository as `owner/name`.
    repository: String,
    /// Pull request number.
    number: i64,
    /// Full head SHA of the waiting review.
    head_sha: String,
}

/// Arguments of the tools that take none.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArguments {}

/// The MCP handler of one request; it calls the engine the CLI commands call.
pub(crate) struct Tools<I, C> {
    /// The engine on the shared store.
    engine: Arc<Engine<I, C>>,
}

impl<I, C> Tools<I, C> {
    /// Creates the handler on `engine`.
    pub(crate) fn new(engine: Arc<Engine<I, C>>) -> Self {
        Self { engine }
    }
}

impl<I, C> ServerHandler for Tools<I, C>
where
    I: Integrations + Send + Sync + 'static,
    C: Clock + 'static,
{
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(super::results::UNTRUSTED_NOTICE)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(
            TOOLS.iter().map(tool).collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let caller = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<Caller>())
            .cloned();
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let outcome = match &caller {
            Some(caller) => {
                self.run(caller, &request.name, &arguments, &context.ct)
                    .await
            }
            None => Err(Failure::code(CLIENT_MISSING)),
        };
        let client = caller.as_ref().map_or("", |caller| caller.name.as_str());
        let logged = arguments.to_string();
        let result = match outcome {
            Ok(value) => {
                tracing::info!(
                    client,
                    tool = %request.name,
                    arguments = %logged,
                    outcome = "ok",
                    "mcp tool call"
                );
                CallToolResult::structured(envelope(value))
            }
            Err(failure) => {
                tracing::warn!(
                    client,
                    tool = %request.name,
                    arguments = %logged,
                    outcome = %failure.code,
                    error = failure.error.as_ref().map(ToString::to_string),
                    "mcp tool call"
                );
                CallToolResult::structured_error(json!({ "error_code": failure.code }))
            }
        };
        Ok(result.into())
    }
}

/// A failed tool call: its error code and the engine error behind it, which only the log shows.
struct Failure {
    /// Error code that the result carries.
    code: String,
    /// The engine error, if any.
    error: Option<Error>,
}

impl Failure {
    /// Returns a failure with `code` and no engine error.
    fn code(code: &str) -> Self {
        Self {
            code: code.to_owned(),
            error: None,
        }
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self {
            code: error_code(&error).to_owned(),
            error: Some(error),
        }
    }
}

impl<I, C> Tools<I, C>
where
    I: Integrations + Send + Sync + 'static,
    C: Clock + 'static,
{
    /// Checks the scope of `caller`, then runs the tool `name`.
    async fn run(
        &self,
        caller: &Caller,
        name: &str,
        arguments: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value, Failure> {
        let Some(spec) = TOOLS.iter().find(|spec| spec.name == name) else {
            return Err(Failure::code(TOOL_UNKNOWN));
        };
        if !caller.scope.allows(spec.scope) {
            return Err(Failure::code(SCOPE_DENIED));
        }
        let engine = &self.engine;
        match spec.name {
            "list_reviews" => {
                let parsed: ListArguments = parse(arguments)?;
                let filter = list_filter(&parsed)?;
                let limit = list_limit(parsed.limit, engine.config.review_list_limit)?;
                crate::discovery::poll::live(cancel)?;
                encode(&engine.store.list_reviews(&filter, limit)?)
            }
            "review_detail" => {
                let parsed: DetailArguments = parse(arguments)?;
                if !identifier_valid(&parsed.workflow_id) {
                    return Err(Failure::code(COMMAND_INVALID));
                }
                encode(&engine.review_detail(cancel, &parsed.workflow_id)?)
            }
            "list_awaiting" => {
                let _: NoArguments = parse(arguments)?;
                encode(&engine.list_awaiting(cancel)?)
            }
            "pull_request_status" => {
                let parsed: PullRequestArguments = parse(arguments)?;
                if !repository_valid(&parsed.repository) || parsed.number <= 0 {
                    return Err(Failure::code(COMMAND_INVALID));
                }
                let status = engine.pull_request_status(cancel, &parsed.repository, parsed.number);
                encode(&status?)
            }
            "review_queue" => {
                let _: NoArguments = parse(arguments)?;
                let (entries, truncated) = engine.queue_snapshot(cancel).await?;
                encode(&queue_view(entries, truncated))
            }
            "resolve" => {
                let parsed: PrefixArguments = parse(arguments)?;
                let request = ResolveRequest {
                    repository: parsed.repository,
                    number: parsed.number,
                    head_prefix: parsed.head_prefix,
                };
                encode(&engine.resolve(cancel, &request)?)
            }
            "approve" => {
                let parsed: ApproveArguments = parse(arguments)?;
                let request = ApproveRequest {
                    repository: parsed.repository,
                    number: parsed.number,
                    head_sha: parsed.head_sha,
                };
                encode(&engine.approve_head(cancel, &request)?)
            }
            "retry" => {
                let parsed: PrefixArguments = parse(arguments)?;
                let request = RetryRequest {
                    repository: parsed.repository,
                    number: parsed.number,
                    head_sha: parsed.head_prefix,
                };
                encode(&engine.retry(cancel, &request).await?)
            }
            _ => Err(Failure::code(TOOL_UNKNOWN)),
        }
    }
}

/// Reads the arguments of a tool; unknown or mistyped fields are refused.
fn parse<T: DeserializeOwned>(arguments: &Value) -> Result<T, Failure> {
    T::deserialize(arguments).map_err(|_| Failure::code(COMMAND_INVALID))
}

/// Encodes a tool result as JSON.
fn encode<T: Serialize>(value: &T) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| Failure::code(RESULT_INVALID))
}

/// Returns the store filter of `list_reviews`.
fn list_filter(arguments: &ListArguments) -> Result<ReviewFilter, Failure> {
    let status_valid = arguments
        .status
        .as_deref()
        .is_none_or(|status| STATUSES.contains(&status));
    let repository_ok = arguments.repository.as_deref().is_none_or(repository_valid);
    if !status_valid || !repository_ok {
        return Err(Failure::code(COMMAND_INVALID));
    }
    Ok(ReviewFilter::Matching {
        status: arguments.status.clone(),
        repository: arguments.repository.clone(),
    })
}

/// Returns the `list_reviews` limit: `limit` when given, otherwise `maximum`, as the CLI does.
fn list_limit(limit: Option<i64>, maximum: i64) -> Result<i64, Failure> {
    match limit {
        None => Ok(maximum),
        Some(value) if value > 0 && value <= maximum => Ok(value),
        Some(_) => Err(Failure::code(COMMAND_INVALID)),
    }
}

/// Returns the MCP description of `spec`, with its input schema and hints.
fn tool(spec: &ToolSpec) -> Tool {
    let read_only = spec.scope == Scope::Read;
    Tool::new(spec.name, spec.description, input_schema(spec.name)).with_annotations(
        ToolAnnotations::new()
            .read_only(read_only)
            .destructive(false)
            .open_world(spec.name == "review_queue" || spec.name == "retry"),
    )
}

/// Returns the JSON schema of the arguments of the tool `name`.
fn input_schema(name: &str) -> JsonObject {
    let repository = json!({ "type": "string", "description": "Repository as owner/name." });
    let number = json!({ "type": "integer", "minimum": 1, "description": "Pull request number." });
    let prefix = json!({
        "type": "string",
        "description": "Head SHA, or a prefix of at least 12 lowercase hexadecimal characters."
    });
    match name {
        "list_reviews" => object(
            &[
                ("status", json!({ "type": "string", "enum": STATUSES })),
                ("repository", repository),
                ("limit", json!({ "type": "integer", "minimum": 1 })),
            ],
            &[],
        ),
        "review_detail" => object(
            &[("workflow_id", json!({ "type": "string" }))],
            &["workflow_id"],
        ),
        "pull_request_status" => object(
            &[("repository", repository), ("number", number)],
            &["repository", "number"],
        ),
        "resolve" | "retry" => object(
            &[
                ("repository", repository),
                ("number", number),
                ("head_prefix", prefix),
            ],
            &["repository", "number", "head_prefix"],
        ),
        "approve" => object(
            &[
                ("repository", repository),
                ("number", number),
                (
                    "head_sha",
                    json!({ "type": "string", "description": "Full 40-character head SHA." }),
                ),
            ],
            &["repository", "number", "head_sha"],
        ),
        _ => object(&[], &[]),
    }
}

/// Builds an object schema that refuses properties it does not list.
fn object(properties: &[(&str, Value)], required: &[&str]) -> JsonObject {
    let mut listed = JsonObject::new();
    for (name, schema) in properties {
        listed.insert((*name).to_owned(), schema.clone());
    }
    let mut schema = JsonObject::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(listed));
    schema.insert("required".to_owned(), json!(required));
    schema.insert("additionalProperties".to_owned(), json!(false));
    schema
}
