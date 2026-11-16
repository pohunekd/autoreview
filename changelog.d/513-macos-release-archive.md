---
kind: added
---
Each release also publishes `autoreview-<version>-aarch64-apple-darwin.tar.gz` for Apple Silicon Macs on macOS 14 or newer, with its `.sha256` file and a build provenance attestation. The binary is ad-hoc signed and not notarized; `scripts/macos-check archive <archive>` checks the checksum, the attestation, the signature and the version on a Mac. ([#513](https://github.com/pohunekd/autoreview/issues/513))
