---
kind: changed
---
On macOS, `pohunek.runtime_dir` may be left out of `config.yaml`: autoreview then passes no `XDG_RUNTIME_DIR` to the pohunek CLI, which reaches its daemon in pohunek's macOS default directory, and `autoreview init` leaves the key out unless `--pohunek-runtime-dir` is given. On Linux the key stays required. ([#513](https://github.com/pohunekd/autoreview/issues/513))
