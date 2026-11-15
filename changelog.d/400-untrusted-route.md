---
kind: added
---
A `pohunek.route_overrides` entry takes an optional `untrusted: {host, project_id}` route: the pull requests of authors that the rule does not trust run only on that host and project, and the trusted authors keep the entry's own route. A failing untrusted host makes the review wait for it and a missing project fails with `project_route_not_found`; neither falls back to the trusted host. Without the key, routing is unchanged.
