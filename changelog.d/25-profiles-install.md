---
kind: added
---
`autoreview profiles install` writes, on the review host it runs on, the pohunek agent profile
of a reviewer and the Claude or Codex adapter that it starts, both embedded in the binary. It
pins the official runtime package, takes the model, the effort, the agent executable and the
adapter limits as required flags, keeps identical files, rewrites the files it wrote earlier and
refuses any other profile of that name unless `--replace` is given. The Codex adapter sets a
terminal title before it reads its request, which pohunek's input readiness for Codex waits for.
