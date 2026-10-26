# public-repositories

Public repositories accept pull requests from strangers. This policy reviews
the organisation's repositories only when a maintainer labels the pull request,
and it never runs a stranger's pull request with approval or with code checked
out from the pull request.

- **`pohunekd/*`:** label `ai:review`; maintainers (permission `maintain` or
  above) get a Sonnet review that can request changes
- **Untrusted authors and forks:** Codex review from the base checkout, comment only
- **`zajca/pohunek`:** same label, write-permission authors only; everyone else is skipped

Read the [guide](../../docs/knowledge/guides/public-repositories.md) for the
walk-through.

## Use it

```sh
autoreview init --dir /etc/autoreview --login <github-login> \
  --pohunek-host <pohunek-host> --example public-repositories
autoreview --config /etc/autoreview/config.yaml validate
```

Install the `autoreview-sonnet` and `autoreview-codex` profiles on the review
host. Projects must be registered with `pohunek project add` on the review host,
because `project_sources: [manual]` only serves registered repositories.

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
