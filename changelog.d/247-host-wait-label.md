---
kind: changed
---
The review queue lists a review that waits for a review host as `AI review waits for a review host: CODE` instead of `AI review queued`, and a host whose outage was announced is asked on every discovery pass until it answers, so a host that recovers while no review waits for it is announced again at its next outage.
