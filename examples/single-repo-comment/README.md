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
autoreview init --dir /etc/autoreview --login <github-login> \
  --pohunek-host <pohunek-host> --example single-repo-comment
autoreview --config /etc/autoreview/config.yaml validate
```

`init` writes `config.yaml`, this `policy.yaml` and the default prompts
`prompts/generic.md` and `prompts/verifier.md`. Install the Codex profile on the
review host first with `autoreview profiles install` (see the
[CLI reference](../../docs/cli.md)).

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
