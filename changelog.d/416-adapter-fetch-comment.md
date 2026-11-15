---
kind: changed
---
The Claude and Codex adapters describe in a comment that pohunek 0.43.0 or later fetches the pull request head at session start and an older pohunek only the base branch. Only the comment changed, so `autoreview profiles install` writes the new adapter when it runs next, and no reinstall is needed.
