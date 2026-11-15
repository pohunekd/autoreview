---
kind: security
---
Reviewer and verifier requests write U+0085 (next line) in pull request discussion, linked issues, prior findings and claims as the JSON escape `\u0085`, like U+2028 and U+2029, so a reader that ends a line at it cannot see a line the engine did not write. The JSON that `autoreview` commands print escapes U+0085 the same way.
