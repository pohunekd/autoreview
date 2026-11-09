# coding-agent-drafts

Coding agents open pull requests as drafts and never mark them ready. This
policy marks their drafts ready when they request a review, then reviews them
like any other pull request.

- **Repository:** `zajca/pohunek` (review requested)
- **Ready drafts from:** `devin-ai-integration`, `copilot-swe-agent`
- **Reviewer:** Sonnet for the agents and other trusted authors; Codex for
  untrusted authors (comment only)
- **Outcome:** blocking findings request changes; a clean review waits for `autoreview approve`

Read the [guide](../../docs/knowledge/guides/coding-agent-drafts.md) for the
walk-through.

## Use it

```sh
mkdir -p /etc/autoreview/prompts
cp examples/config.yaml /etc/autoreview/config.yaml
cp examples/coding-agent-drafts/policy.yaml /etc/autoreview/policy.yaml
cp prompts/generic.md prompts/verifier.md /etc/autoreview/prompts/
autoreview --config /etc/autoreview/config.yaml validate
```

Set the keys that [use one](../README.md#use-one) names first.

Install the `autoreview-sonnet` and `autoreview-codex` profiles on the review
host. Replace the logins in `ready_draft_authors` and `authors.trusted.logins`
with the coding agents your repository uses.

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
