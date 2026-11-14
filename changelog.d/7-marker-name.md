---
kind: added
---
`publish.marker` names the two hidden markers of every published review: `autoreview` writes `<!-- autoreview:v1:...-->`, and `hermes-pr-review`, also used when the key is absent, keeps `<!-- hermes-pr-review:v1:...-->` for merge gates that read it. A retry after a lost acknowledgement finds the review under either name, so changing the key never posts a review twice.
