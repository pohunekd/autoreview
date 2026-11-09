---
kind: fixed
---
An unreachable notifier no longer holds or fails a review: each notification of a review gets
one attempt, and an undelivered one is logged as `review notification dropped` while the review
goes on without resending it. `autoreview review-queue` posts its digest once and fails with the
backend's error code, logged as `review queue digest dropped`, instead of retrying for the whole
step budget.
