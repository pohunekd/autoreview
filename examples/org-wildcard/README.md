# org-wildcard

One organisation, three rules: two repositories with their own pipelines and
prompts, then every other repository of the organisation with a generic
pipeline.

- **`pohunekd/pohunek-companion`:** Sonnet with `prompts/companion.md` (Rust and Slint)
- **`pohunekd/pohunek-companion-android`:** Sonnet with `prompts/android.md` (Kotlin)
- **`pohunekd/*`:** Codex with the generic prompt; clean reviews are approved
- **Trigger:** the `ai:review` label on all three rules
- **Authors:** write-permission authors only on the trusted side; the generic
  rule reviews other authors with a comment

The first match wins. If the `pohunekd/*` rule were placed above the two
specific rules, the validator would stop with
`unreachable, pohunekd/* matches first`.

Read the [guide](../../docs/knowledge/guides/org-wildcard.md) for the
walk-through.

## Use it

`autoreview init` writes only the generic and verifier prompts, so copy the two
extra prompts from this example into the policy directory as well:

```sh
autoreview init --dir /etc/autoreview --login <github-login> \
  --pohunek-host <pohunek-host> --example org-wildcard
cp examples/org-wildcard/prompts/companion.md examples/org-wildcard/prompts/android.md \
  /etc/autoreview/prompts/
autoreview --config /etc/autoreview/config.yaml validate
```

Install the `autoreview-sonnet` and `autoreview-codex` profiles on the review
host. The generic pipeline approves clean reviews, which the validator accepts
because its incremental reviewers are the same as its full reviewers.

Files: [policy.yaml](policy.yaml), [prompts/](prompts/), shared [config.yaml](../config.yaml).
