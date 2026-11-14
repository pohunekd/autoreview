---
kind: changed
---
Each `matrix` backend of `notify.backends` uses its own homeserver, token and mentioned user, so routes may reach Matrix backends with different identities. A `notify.review_queue.backends` list that names a backend twice is refused at load, and a route to a `none` backend is refused with `notify.routes.<key>: none backends are not routed until #29`.
