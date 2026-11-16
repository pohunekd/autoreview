---
kind: added
---
`autoreview init --notifier none|matrix|webhook` chooses the one backend that every notification level and the review queue go to: a `none` backend that only logs each message, the placeholder `matrix` backend of the reference file, or a `webhook` backend whose URL is read from `AUTOREVIEW_WEBHOOK_URL` and whose format `--webhook-format json|slack|discord` gives. Without `--notifier`, `init` writes the `matrix` backend as before; the Matrix flags are refused with `none` or `webhook`, and `--webhook-format` is required by `webhook` and refused otherwise, each with `configuration_invalid`.
