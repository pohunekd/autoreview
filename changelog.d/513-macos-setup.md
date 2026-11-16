---
kind: added
---
`scripts/setup` runs on a Mac with Apple Silicon and macOS 14 or newer, with bash 4 or newer (`brew install bash`). It installs the `aarch64-apple-darwin` archives of autoreview and pohunek after their checksum and attestation checks, writes the configuration without `pohunek.runtime_dir`, refuses configuration and XDG directories in the folders that macOS privacy protection closes to launchd jobs, and checks that the pohunek daemon finds Python 3.11 or newer for the reviewer adapters. ([#513](https://github.com/pohunekd/autoreview/issues/513))
