---
kind: fixed
---
A review queue routed only to `none` backends is refused at load with
`notify.review_queue.backends: none is not supported for the review queue`, and an empty list
with `notify.review_queue.backends: must name at least one backend`, in place of the unrelated
`invalid Matrix queue room`.
