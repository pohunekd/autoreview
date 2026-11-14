---
kind: added
---
`validate` and `serve` run `pohunek --version` on the CLI at `pohunek.bin`: `validate` prints `pohunek_version` and `serve` logs it as `pohunek version checked`. A release outside the supported range, 0.41.0 or later and before 1.0.0, fails with `pohunek_version_unsupported`, and a run that fails or prints no `pohunek X.Y.Z` with `pohunek_version_invalid`; both name `pohunek.bin`.
