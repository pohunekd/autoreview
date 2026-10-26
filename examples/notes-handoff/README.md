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
autoreview init --dir /etc/autoreview --login <github-login> \
  --pohunek-host <pohunek-host> --example notes-handoff
autoreview --config /etc/autoreview/config.yaml validate
```

Install the `autoreview-sonnet` profile on the review host. The `notes_handoff`
section is required by the validator because a rule sets `on_notes`; remove both
the `on_notes` line and the section to turn the hand-off off.

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
