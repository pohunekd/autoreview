---
kind: added
---
A repository rule can bound the reviews started for one pull request author with `author_limit` (`reviews`, `window_hours`, `over: wait` or `skip`). A start over the bound waits until the window frees a slot, or is skipped with one `review_stopped` notice (`author_limit_reached`) until a review of the pull request starts or fails, also across a wait for a review host; each held start logs `review held by the author limit`. A policy without the key keeps its digest and behaves as before.
