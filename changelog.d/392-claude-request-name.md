---
kind: changed
breaking: true
---
Claude reviewer and verifier requests are now sent under the protocol name `autoreview-claude/v1` instead of `claude-otel-noninteractive/request-v1`, and the answer of a Claude adapter from 0.3.0 or earlier, which runs such a request as a plain prompt and answers in mode `text`, now stops the review with `review_adapter_outdated` instead of a retried `review_result_invalid`. Run `autoreview profiles install` for every Claude profile on every review host before this release is deployed; a review stopped on a missed host needs that reinstall and `autoreview retry`, and a rollback of the engine needs no reinstall.
