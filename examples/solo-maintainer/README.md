# solo-maintainer

For a maintainer who reviews their own and other people's pull requests with
the same service.

- **Repositories:** `zajca/pohunek` (review requested), `zajca/pohunek-work` (`ai:review` label)
- **Reviewer:** Sonnet for trusted authors, Codex for untrusted ones
- **Trusted:** the login `zajca`, or anyone with write permission
- **Own pull requests:** always get a comment, because GitHub does not let an
  author approve or request changes on their own pull request
- **Contributors:** trusted contributors get `request_changes` or `approve`;
  everyone else is reviewed with a comment only

The core repository has one trigger, `review-requested`, and the work
repository has a label trigger. A repository has one trigger per rule, and a
second rule for the same repository is rejected as unreachable.

Read the [guide](../../docs/knowledge/guides/solo-maintainer.md) for the
walk-through.

## Use it

```sh
autoreview init --dir /etc/autoreview --login <github-login> \
  --pohunek-host <pohunek-host> --example solo-maintainer
autoreview --config /etc/autoreview/config.yaml validate
```

Install the `autoreview-sonnet` and `autoreview-codex` profiles on the review
host before the first review. Replace `zajca` with your own login in
`logins` and in the `match` values.

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
