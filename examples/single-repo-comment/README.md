# single-repo-comment

The simplest useful setup: one repository, one reviewer, comments only.

- **Repository:** `zajca/pohunek`
- **Trigger:** the `ai:review` label
- **Reviewer:** Codex (`codex-envelope`), one pipeline, read-only, base checkout.
  The pull request head must already be in the project repository on the review
  host (`git fetch origin refs/pull/<n>/head` there, until
  [#253](https://github.com/pohunekd/autoreview/issues/253)); see
  [Codex checkout refused](../../docs/knowledge/runbooks/troubleshooting.md#codex-checkout-refused)
- **Outcome:** every review is a comment. Nothing is approved or blocked.
- **Authors:** `trusted: any`, because a comment cannot merge or block anything.

Use it to try autoreview on one repository before you give it more authority.
Read the [guide](../../docs/knowledge/guides/single-repo-comment.md) for the
walk-through.

## Use it

```sh
autoreview init --dir /etc/autoreview --login <login> --pohunek-host <host> \
  --example single-repo-comment
autoreview --config /etc/autoreview/config.yaml validate
```

`init` writes the config, this policy and its prompts, and never overwrites a
file. To copy them by hand instead:

```sh
mkdir -p /etc/autoreview/prompts
cp examples/config.yaml /etc/autoreview/config.yaml
cp examples/single-repo-comment/policy.yaml /etc/autoreview/policy.yaml
cp prompts/generic.md prompts/verifier.md /etc/autoreview/prompts/
autoreview --config /etc/autoreview/config.yaml validate
```

Set the keys that [use one](../README.md#use-one) names first.

Create the `autoreview-codex` profile on the review host first, with
`autoreview profiles install --host <HOST> --agent codex --name autoreview-codex`
and the flags of [the CLI](../../docs/cli.md#autoreview-profiles-install), run on
that host.

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
