---
kind: added
---
`serve` and the other engine commands accept several credentials in `github.credentials`, so repositories can be reviewed with a fine-grained token per owner or repository, or by another account per owner. With several credentials each credential's search, in a discovery pass and in the review queue, gets one attempt, so a failing token does not delay the repositories of the others.
