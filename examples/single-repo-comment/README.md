# single-repo-comment

The simplest useful setup: one repository, one reviewer, comments only.

- **Repository:** `zajca/pohunek`
- **Trigger:** the `ai:review` label
- **Reviewer:** Codex (`codex-envelope`), one pipeline, read-only, base checkout
- **Outcome:** every review is a comment. Nothing is approved or blocked.
- **Authors:** `trusted: any`, because a comment cannot merge or block anything.

Use it to try autoreview on one repository before you give it more authority.
Read the [guide](../../docs/knowledge/guides/single-repo-comment.md) for the
walk-through.

## Use it

```sh
mkdir -p /etc/autoreview/prompts
cp examples/config.yaml /etc/autoreview/config.yaml
cp examples/single-repo-comment/policy.yaml /etc/autoreview/policy.yaml
cp prompts/generic.md prompts/verifier.md /etc/autoreview/prompts/
autoreview --config /etc/autoreview/config.yaml validate
```

Set the keys that [use one](../README.md#use-one) names first.

Create the `autoreview-codex` profile on the
review host first; `autoreview profiles install` is planned in #25.

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
