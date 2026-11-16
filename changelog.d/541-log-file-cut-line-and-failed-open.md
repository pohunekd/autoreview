---
kind: fixed
---
The optional log file no longer keeps a partial line after a failed write (a full disk): the cut piece is truncated away, and when that fails too, the next line starts on a new line so it stays valid JSON. A rotation creates the new file before it renames anything, so a failed create leaves the current file in place and no `<file>` gap appears.
