---
kind: changed
breaking: true
---
`session.name_prefix` and `session.branch_prefix` now name the pohunek sessions and reviewer branches. Set them to `github-review-` and `autoreview/review/` to keep the names of earlier releases, and change them only while no review runs, because a running review finds its session by name. A `session.branch_prefix` that cannot be part of a git ref name, or a prefix with a control character, is refused with `configuration_invalid`. `publish.signature` is published as written, so the example no longer shows a `{login}` placeholder.
