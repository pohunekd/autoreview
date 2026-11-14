---
kind: added
---
Notifications can go to a `none` backend, which only logs each message, and to a `webhook` backend that posts to Slack, Slack-compatible services, Discord or your own JSON receiver; the webhook URL is read from the variable that `url_env` names and never appears in logs or errors. A webhook request follows no redirect and sends no `Referer`, so a `3xx` answer fails with `webhook_request_rejected`.
