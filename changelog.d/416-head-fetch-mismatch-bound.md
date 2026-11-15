---
kind: fixed
---
A base-checkout review whose fetched `refs/pull/<n>/head` stays at another commit than the pull request head no longer starts again without end and without a notice: the start ends as `pohunek_head_fetch_mismatch`, the head starts again after the debounce once for each entry in `schedule.retry_delays_seconds`, and the next consecutive mismatch stops it with a `review_stopped` notification that names the expected and the fetched commit. A head whose ref catches up is reviewed as before, and a new push starts its own count.
