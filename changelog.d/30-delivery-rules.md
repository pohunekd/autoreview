---
kind: added
---
A notification route and `notify.review_queue.backends` may name several backends; each backend gets every message once, a failing backend no longer stops the others, and a review resumed after a shutdown sends a message only to the backends that have not taken it. The warnings of dropped notifications name the backend.
