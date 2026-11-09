//! The MCP server against the fake integration: list, resolve and approve over HTTP.

// Rust guideline compliant 2026-06-26

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use autoreview_policy::Level;
use bytes::Bytes;
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Full};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::engine::{Engine, SystemClock};
use crate::mcp::{MCP_PATH, McpClientEntry, McpConfig, McpLimits, McpServer};
use crate::messages::APPROVAL_REQUESTED_TITLE;
use crate::protocol::fixtures::sha;
use crate::store::{REVIEW_AWAITING, Store};
use crate::testing::fake::FakeIntegration;
use crate::testing::harness::{HEAD_PREFIX_CHARS, integration_pull_request, start_harness};

/// Token of the client with the read scope.
const READ_TOKEN: &str = "read-token-0123456789abcdef0123456789abcdef";

/// Token of the client with the write scope.
const WRITE_TOKEN: &str = "write-token-0123456789abcdef0123456789abcdef";

/// The `Host` header that the server accepts.
const HOST: &str = "mcp.example.com";

/// Returns the client entry `name` with the digest of `token`.
fn client(name: &str, scope: &str, token: &str) -> McpClientEntry {
    McpClientEntry {
        name: name.to_owned(),
        scope: scope.to_owned(),
        token_sha256: hex::encode(Sha256::digest(token.as_bytes())),
    }
}

/// Posts one JSON-RPC `tools/call` and returns the status and the JSON body.
async fn call(
    server: &McpServer<FakeIntegration, SystemClock>,
    token: Option<&str>,
    tool: &str,
    arguments: Value,
) -> (StatusCode, Value) {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    });
    let mut builder = Request::post(MCP_PATH)
        .header("host", HOST)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = builder
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("build the request");
    let response = server.handle(request).await;
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("the body is JSON")
    };
    (status, value)
}

/// Returns the structured content of a tool result and whether it is an error.
fn structured(body: &Value) -> (&Value, bool) {
    let result = &body["result"];
    (
        &result["structuredContent"],
        result["isError"] == json!(true),
    )
}

#[tokio::test]
async fn mcp_lists_resolves_and_approves_with_one_publication() {
    let h = start_harness(|_| {}).await;
    let pr = integration_pull_request("acme/manual", &sha("a"));
    h.fake.open(pr.clone(), h.rule(&pr.repository));
    h.poll_until("the approval request", || {
        h.fake.has_note(Level::Action, APPROVAL_REQUESTED_TITLE)
    })
    .await;
    let config = McpConfig::new(
        "127.0.0.1:0",
        vec![HOST.to_owned()],
        &[
            client("reader", "read", READ_TOKEN),
            client("writer", "write", WRITE_TOKEN),
        ],
        McpLimits {
            request_timeout: Duration::from_secs(30),
            header_timeout: Duration::from_secs(10),
            request_max_bytes: 65_536,
            max_connections: 4,
        },
    )
    .expect("the MCP settings are valid");
    // The server runs as its own process in production: its own store and engine on the file.
    let cfg = h.config();
    let store = Store::open(
        Path::new(&cfg.state_db_path),
        Box::new(OffsetDateTime::now_utc),
    )
    .expect("open the store a second time");
    let engine = Engine::new(cfg, Arc::new(store), Arc::clone(&h.fake), SystemClock);
    let server = McpServer::new(Arc::new(engine), &config, CancellationToken::new());

    let (status, _) = call(&server, None, "list_awaiting", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a call without a token");
    let (status, _) = call(&server, Some("other-token"), "list_awaiting", json!({})).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a call with an unknown token"
    );

    let filter = json!({ "repository": "acme/manual", "status": REVIEW_AWAITING });
    h.wait_for_async("the awaiting review in list_reviews", async || {
        let (_, body) = call(&server, Some(READ_TOKEN), "list_reviews", filter.clone()).await;
        structured(&body).0["result"]
            .as_array()
            .is_some_and(|reviews| reviews.len() == 1)
    })
    .await;
    let (_, body) = call(&server, Some(READ_TOKEN), "list_reviews", filter).await;
    let listed = structured(&body).0;
    assert!(
        listed["untrusted_data_notice"].is_string(),
        "every result carries the untrusted-data notice: {listed}"
    );
    let workflow_id = listed["result"][0]["workflow_id"]
        .as_str()
        .expect("the review has a workflow ID")
        .to_owned();

    let approve = json!({
        "repository": pr.repository,
        "number": pr.number,
        "head_sha": pr.head_sha,
    });
    let (_, body) = call(&server, Some(READ_TOKEN), "approve", approve.clone()).await;
    let (content, failed) = structured(&body);
    assert!(
        failed && content["error_code"] == json!("mcp_scope_denied"),
        "a read client must not approve: {body}"
    );

    let resolve = json!({
        "repository": pr.repository,
        "number": pr.number,
        "head_prefix": pr.head_sha[..HEAD_PREFIX_CHARS],
    });
    let (_, body) = call(&server, Some(WRITE_TOKEN), "resolve", resolve).await;
    let (content, failed) = structured(&body);
    assert!(
        !failed
            && content["result"]["workflow_id"] == json!(workflow_id)
            && content["result"]["head_sha"] == json!(pr.head_sha),
        "resolve = {body}"
    );

    let (_, body) = call(&server, Some(WRITE_TOKEN), "approve", approve).await;
    let (content, failed) = structured(&body);
    assert!(
        !failed && content["result"]["publication_status"] == json!("pending"),
        "approve = {body}"
    );
    h.wait_for("the approval", || h.publications() == 1).await;

    let detail = json!({ "workflow_id": workflow_id });
    h.wait_for_async("the publication in review_detail", async || {
        let (_, body) = call(&server, Some(READ_TOKEN), "review_detail", detail.clone()).await;
        structured(&body).0["result"]["publication"]["review_url"]
            .as_str()
            .is_some_and(|url| !url.is_empty())
    })
    .await;
    let (_, body) = call(&server, Some(READ_TOKEN), "review_detail", detail).await;
    let shown = structured(&body).0;
    assert_eq!(
        shown["result"]["decision"],
        json!("APPROVE"),
        "detail = {shown}"
    );
    h.settle().await;
    assert_eq!(h.publications(), 1, "the approval published more than once");
}
