---
kind: fixed
---
A pull request whose review waits for approval no longer stops every discovery pass when its direct read fails: before, one such pull request (for example one moved to a base branch that autoreview does not support) failed each pass with `discovery poll failed`, and no other pull request in any repository got a review. Now a wait whose pull request the search rejects as `base_branch_unsupported` or `review_request_event_missing` ends as `superseded`, and any other read error is logged as `approval wait unreadable` and keeps the wait for the next pass.
