---
kind: changed
breaking: true
---
Each `matrix` backend of `notify.backends` uses its own homeserver, token and mentioned user, so routes may reach Matrix backends with different identities. Every `matrix` backend is now validated at load, also one that no route or queue names, and an invalid one is refused with an error that names `notify.backends.<name>` and the key. A `notify.review_queue.backends` list that names a backend twice is refused, and a route to a `none` backend is refused with `notify.routes.<key>: none backends are not routed until #29`.
