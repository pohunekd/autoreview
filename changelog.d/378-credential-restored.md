---
kind: added
---
A suspended credential whose token GitHub accepts again raises the new `credential_restored` event (optional in the policy, default `action`) once per lift, so the owner learns that the repositories of that credential are reviewed again without a restart. A policy without the event keeps its digest.
