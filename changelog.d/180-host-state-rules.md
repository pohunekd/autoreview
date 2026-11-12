---
kind: added
---
The optional `pohunek.host_state` section picks review hosts by the state that the `moc.host-state` pohunek plugin reports: it can skip a host on battery, below a battery charge, below a free memory limit, without a required local model or outside its policy window, try idle hosts first, and decide what a host whose state is unknown or stale does. Each host's state is read at most once per discovery pass. When a rule held back a host and no allowed host has the repository, the review waits for a review host with `host_state_ineligible` instead of failing with `project_route_not_found`. A repository pinned by `route_overrides` is not filtered. Without the section nothing changes.
