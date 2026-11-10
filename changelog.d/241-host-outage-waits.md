---
kind: fixed
---
A review that is due while every review host fails waits for a host instead of using its
retries: it starts on the first discovery pass after a host answers, without `autoreview retry`,
and one `review_stopped` message per outage says that reviews wait for a review host.
