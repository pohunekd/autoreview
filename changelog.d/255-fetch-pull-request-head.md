---
kind: added
---
A `checkout: base` review fetches the pull request head when its session starts, so fork pull requests and freshly pushed branches are reviewed without a manual `git fetch`; this needs pohunek 0.43.0 or later as the CLI and on the review host, and with an older pohunek reviews start as before. A failed fetch stops the start as `pohunek_head_fetch_failed` (retried) or `pohunek_head_fetch_ref_missing`, and a fetched head at another commit than the pull request head stops it as `pohunek_head_fetch_mismatch`, which starts again after the debounce once for each entry in `schedule.retry_delays_seconds` and then stops with a `review_stopped` notification that names the expected and the fetched commit; a new push starts its own count.
