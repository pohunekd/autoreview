---
kind: added
---
A credential can list teams in `review_teams`; a pull request that requests its review from one of them is reviewed as if it requested the credential's login. Without the key, a team-only request is still rejected as `review_requested_from_team`.
