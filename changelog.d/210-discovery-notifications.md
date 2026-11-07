---
kind: fixed
---
An unreachable notifier no longer stalls discovery: a discovery pass saves its state and queues
due reviews first, then sends each notification once, skips the rest of a level after one fails,
and logs every undelivered one as `discovery notification dropped` without resending it. Every
retry of a step is logged as `step failed, retrying` with the step, the error code and the delay.
