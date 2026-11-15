---
kind: security
---
The threat model no longer says that a review host project without setup hooks protects a `checkout: head` review: pohunek reads the in-repo hooks (`.pohunek/hooks/<event>`, `.pohunek/setup`) from the review worktree, so on a head checkout every in-repo hook, and any code it runs, is the pull request's code and runs on the review host. Give a head checkout only to authors who may run code on the review host; `checkout: base`, which every untrusted author gets, keeps the pull request's hooks out of the worktree.
