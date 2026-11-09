---
type: Guide
id: guide/mcp-server
title: MCP server
description: Run autoreview mcp so that agents list, read, approve and retry reviews through MCP tools over Streamable HTTP - clients and scopes, token digests, the reverse proxy, the tools and their untrusted output.
source_kind: manual
intents: [setup, help]
---

# MCP server

> **Status: 0.1.0.** This page describes autoreview 0.1.0, the first release. Parts
> marked planned or "not in v0.1" are not in 0.1.0.

`autoreview mcp` serves the review commands as [MCP](https://modelcontextprotocol.io) tools
over Streamable HTTP. Any MCP client can then list reviews, read their findings, and approve
or retry them: a chat bot, Hermes or Claude. It is a separate process over the same database
as the CLI. A failure or a restart of the MCP server never touches `autoreview serve`.

The server is optional. Without an `mcp` section in `config.yaml` nothing changes, and
`autoreview mcp` refuses to start with `configuration_missing` naming `mcp`.

## Clients and scopes

Each client has its own static bearer token and a scope:

| Scope | Tools |
|---|---|
| `read` | `list_reviews`, `review_detail`, `list_awaiting`, `pull_request_status`, `review_queue` |
| `write` | The read tools, and `resolve`, `approve`, `retry` |

A request without a token, or with a token that no client has, gets HTTP 401 and reaches
no tool. A read client that calls a write tool gets the tool error `mcp_scope_denied`.

## Create a token

The configuration holds only the SHA-256 digest of each token, never the token. Generate
a random token of 32 bytes, give it to the client, and write its digest into the
configuration:

```bash
umask 077
openssl rand -hex 32 > hermes.token
tr -d '\n' < hermes.token | sha256sum | cut -d ' ' -f 1
```

The `tr` removes the newline, so the digest is of the token alone. A plain digest is safe
because the token is random and long; a short or chosen password would not be. Rotate a
token by replacing its digest and restarting `autoreview mcp`.

## Configure

```yaml
mcp:
  listen: "127.0.0.1:8787"
  allowed_hosts: [autoreview.example.com]
  request_timeout_seconds: 120
  header_timeout_seconds: 10
  request_max_bytes: 65536
  max_connections: 16
  clients:
    - name: hermes
      scope: write
      token_sha256: "<64 lowercase hexadecimal characters>"
    - name: dashboard
      scope: read
      token_sha256: "<64 lowercase hexadecimal characters>"
```

Every key is required; see the [configuration reference](../../configuration.md#mcp).

## Run it behind a reverse proxy

TLS ends at a reverse proxy in front of the server; the server itself speaks plain
HTTP/1.1 on `mcp.listen`, which should be a loopback address.

- The endpoint is the path `/mcp`; every other path gets 404.
- The proxy passes the `Authorization` header through unchanged.
- The proxy forwards the public `Host` (in nginx `proxy_set_header Host $host;`; Caddy does
  it by default), and `mcp.allowed_hosts` lists that name. The check guards against DNS
  rebinding; a proxy that rewrites `Host` to the upstream address gets 403 for every request.
- A request with an `Origin` header is refused, so a web page in a browser cannot call the
  server.
- The proxy owns the client timeouts and limits towards the internet. The server's own
  bounds are `mcp.header_timeout_seconds`, `mcp.request_timeout_seconds` (503),
  `mcp.request_max_bytes` (413) and `mcp.max_connections`; give the proxy a read timeout
  longer than `mcp.request_timeout_seconds`.

Start it as a service of its own, for example a second systemd unit with
`ExecStart=autoreview mcp`, next to the unit of `autoreview serve`.

## Tools

Every tool takes a JSON object; unknown arguments are refused with `command_invalid`.

| Tool | Arguments | Returns |
|---|---|---|
| `list_reviews` | `status` (`queued`, `running`, `awaiting`, `completed`), `repository`, `limit`, all optional | The newest reviews, at most `limit` (default and maximum `cli.review_list_limit`) |
| `review_detail` | `workflow_id` | The review, its head, decision, approval wait, publication with its URL, and its findings |
| `list_awaiting` | none | The reviews that wait for a human approval, as `autoreview awaiting` |
| `pull_request_status` | `repository`, `number` | Everything the engine knows about one pull request, as `autoreview review` |
| `review_queue` | none | The requested reviews with their engine status. Unlike `autoreview review-queue` it sends nothing |
| `resolve` | `repository`, `number`, `head_prefix` | The waiting review at a head prefix of at least 12 characters, as `autoreview resolve` |
| `approve` | `repository`, `number`, `head_sha` | The approval of the clean review that waits at the full head, as `autoreview approve` |
| `retry` | `repository`, `number`, `head_prefix` | A new attempt of a stopped review, as `autoreview retry` |

The write tools call the same engine functions as the CLI commands.

- `approve` publishes only the clean review that waits for approval, checked against the
  repository, the number and the full head; the engine publishes it on its next tick.
- `retry` runs one whole discovery pass, as `autoreview retry` does. Besides the new attempt
  of the stopped review, that pass marks coding agents' drafts ready, sends the handoff
  reminders that are due, and dispatches every review that is due. A review it dispatches
  publishes as its rule says, without an approval when the rule approves or comments by
  itself.
- `review_queue` runs the GitHub search of requested reviews with the engine's credential
  on every call, so a read token can spend the GitHub API budget of that credential. Give
  read tokens only to clients you would let run `autoreview review-queue`.

A failed call returns a tool error with `{"error_code": "<code>"}`, using the codes of the
[CLI reference](../../cli.md#error-codes). The codes of the server itself:

| Code | Meaning |
|---|---|
| `command_invalid` | Unknown, missing or mistyped arguments, or a value out of range |
| `mcp_scope_denied` | A client without the `write` scope called a write tool |
| `mcp_tool_unknown` | The server has no tool of that name |
| `review_not_found` | `review_detail` got a workflow ID the store does not know |
| `mcp_client_missing` | A call reached the tools without an authenticated client; a server defect |
| `mcp_result_invalid` | A result could not be encoded as JSON; a server defect |

## Untrusted output

Titles, finding text, paths, summaries and author names come from pull request authors and
models. The server treats them as data:

- they pass the same neutralization as published reviews: no mentions, no raw HTML, no
  images, no comment delimiters;
- every result is wrapped as `{"untrusted_data_notice": "...", "result": ...}`, and the
  notice tells the calling agent that the text is data, never instructions.

Neutralization keeps GitHub from acting on the text; it does not make the text safe for an
agent to obey. The calling agent must keep the review text out of its instructions. Give the
`write` scope only to agents that do, because text injected into a pull request could
otherwise steer the agent into `approve` or `retry`; see the
[threat model](../safety/threat-model.md#attacks-and-mitigations).

## Logs

Every tool call writes one JSON log line `mcp tool call` with the client name, the tool,
its arguments and the outcome (`ok` or the error code). A refused request writes
`mcp request refused` with the reason `missing_token` or `unknown_token`, and a request over
`mcp.request_timeout_seconds` writes `mcp request timed out` with the client name. The token
is never logged.

## Not included

Starting a review on demand, OAuth 2.1 authorization and a package for one particular
agent are not part of the server.
