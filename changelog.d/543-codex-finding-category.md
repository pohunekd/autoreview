---
kind: added
---
Reviewers of the `codex-envelope` kind may give each finding the optional `category` slug that `claude-structured` reviewers already can, so the `guidance` suggestions count their fixed findings too, and a `category` that is not a slug is dropped with one warning. Install the Codex profiles again with `autoreview profiles install`; an engine from before this release refuses a Codex report whose finding carries a category, so install them right before the restart.
