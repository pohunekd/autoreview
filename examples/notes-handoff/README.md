# notes-handoff

Review findings that are only notes do not go straight to approval. The author
gets a comment first, and approval waits until the author asks for a review
again.

- **Repository:** `pohunekd/pohunek-companion` (review requested)
- **Reviewer:** one Sonnet reviewer
- **Outcome:** blocking findings request changes; notes-only reviews are
  comments; a clean review waits for `autoreview approve`
- **Hand-off reminder:** `notes_handoff.reminder_hours: 24`, one reminder for an
  unanswered hand-off
- **Authors:** write-permission authors only

Read the [guide](../../docs/knowledge/guides/notes-handoff.md) for the
walk-through.

## Use it

```sh
autoreview init --dir /etc/autoreview --login <login> --pohunek-host <host> \
  --example notes-handoff
autoreview --config /etc/autoreview/config.yaml validate
```

`init` writes the config, this policy and its prompts, and never overwrites a
file. To copy them by hand instead:

```sh
mkdir -p /etc/autoreview/prompts
cp examples/config.yaml /etc/autoreview/config.yaml
cp examples/notes-handoff/policy.yaml /etc/autoreview/policy.yaml
cp prompts/generic.md prompts/verifier.md /etc/autoreview/prompts/
autoreview --config /etc/autoreview/config.yaml validate
```

Set the keys that [use one](../README.md#use-one) names first.

Install the `autoreview-sonnet` profile on the review host. The `notes_handoff`
section is required by the validator because a rule sets `on_notes`; remove both
the `on_notes` line and the section to turn the hand-off off.

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
