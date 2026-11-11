---
kind: fixed
---
A pull request retargeted away from the base branch of a failed review and back before a new review runs no longer notifies the same failure twice or restarts its automatic retries; a review stopped by that failure stays stopped, and its pending retry waits one retry delay after the return.
