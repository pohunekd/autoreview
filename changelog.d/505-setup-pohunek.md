---
kind: changed
---
`scripts/setup` installs pohunek 0.43.1 when pohunek is missing or older than 0.41.0, so a fresh setup can fetch the pull request head at session start and run head-checkout reviews. A pohunek of 0.41.0 or later that is already installed is kept. ([#505](https://github.com/pohunekd/autoreview/issues/505))
