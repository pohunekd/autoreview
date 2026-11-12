---
kind: added
---
The optional `pohunek.host_state` section picks review hosts by the state that the `moc.host-state` pohunek plugin reports, read once per host and discovery pass: it can skip a host on battery, below a battery charge or free memory limit, without a required local model or outside its policy window, try idle hosts first, and decide what a host with an unknown state does. When only a held-back host has the repository, the review waits for a review host with `host_state_ineligible`; a repository pinned by `route_overrides` is not filtered.
