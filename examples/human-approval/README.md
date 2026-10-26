# human-approval

Two Claude reviewers and a verifier review the core repository. A clean review
waits until a person approves the exact head commit from the CLI.

- **Repository:** `zajca/pohunek` (review requested)
- **Reviewers:** Opus (full review and incremental re-review) and Sonnet (full review)
- **Verifier:** Opus re-checks blocking findings; escalation on critical findings
- **Outcome:** blocking findings request changes; a clean review waits for
  `autoreview approve`
- **Authors:** write-permission authors only; other authors are skipped

Read the [guide](../../docs/knowledge/guides/human-approval.md) for the
walk-through.

## Use it

```sh
autoreview init --dir /etc/autoreview --login <github-login> \
  --pohunek-host <pohunek-host> --example human-approval
autoreview --config /etc/autoreview/config.yaml validate
```

Install the `autoreview-opus` and `autoreview-sonnet` profiles on the review
host. Set `approval.timeout_hours` to how long a clean review may wait for you.

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
