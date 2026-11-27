---
kind: fixed
---
A pull request that also requests a review from a team no longer stops the discovery of every other pull request of its credential when the token lacks the organization permission Members: read, and a pull request requested only from a team is listed as rejected with `review_requested_from_team` instead of being dropped silently.
