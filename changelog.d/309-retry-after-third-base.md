---
kind: fixed
---
A pull request retargeted back to the base branch of a failed review, after a review of the same head completed on another base branch, is no longer taken as reviewed there: the review stays stopped or its pending retry still runs, and `autoreview retry` starts a new attempt instead of answering `review_retry_not_dispatched`.
