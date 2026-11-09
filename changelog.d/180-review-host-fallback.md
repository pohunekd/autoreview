---
kind: changed
---
The review host is the first host of `pohunek.hosts` whose projects serve the repository, so a repository registered on several hosts is reviewed there instead of failing with `project_route_ambiguous`. A host that fails is skipped with the warning `review host skipped` and the next host is asked, so one host outage no longer stops the reviews of repositories that other hosts serve. A repository that a trusted host also serves, for example through an `auto` project source, now runs there instead of failing, so pin untrusted repositories with `route_overrides` or keep `auto` out of their `project_sources`.
