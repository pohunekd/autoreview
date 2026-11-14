---
kind: added
---
A notification route and `notify.review_queue.backends` may name several backends; a failing backend no longer stops the others, and on a route with several backends a review resumed after a shutdown sends an `action` message only to the backends that have not taken it. The warnings of dropped notifications name the backend.
