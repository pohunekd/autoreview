---
kind: changed
---
A request that the MCP server refuses for its `Host` keeps its status, 400 or 403, but gets an empty body instead of a text such as `Forbidden: Host header is not allowed`. Its log line is `mcp request refused` with the reason `host_invalid` or `host_not_allowed` and the received `host`, instead of a `rejected request with ... Host header` line of the MCP library.
