---
kind: fixed
---
An approval sent right after the approval request is accepted instead of failing with
`review_not_awaiting_approval`, because the review is stored as waiting before the request is
sent. When the approval request cannot be delivered, the review keeps waiting and
`autoreview awaiting` lists it, instead of failing the review.
