---
kind: security
---
Claude reviewer and verifier runs now pass `--strict-mcp-config`, so they load no MCP server: before, a server registered in the Claude user scope of the review host (for example `autoreview mcp` with a `write` token, or one with mail or chat access) or by a plugin could be loaded into a run that reads untrusted pull request text, and a `.mcp.json` that a `checkout: head` pull request adds is now skipped too. A base profile that sets `--strict-mcp-config` itself is now refused as a wrapper-controlled option. The change reaches a review host only when its profiles are installed again with `autoreview profiles install`.
