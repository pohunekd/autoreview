---
kind: added
---
An optional `log` section of `config.yaml` (`directory`, `max_file_bytes` and `kept_files`, all required) makes `serve`, `review-queue` and `mcp` write their log lines to `autoreview.log`, `autoreview-queue.log` and `autoreview-mcp.log` in that directory and rotate each file by size, without an external rotation tool. With the file in use, stderr carries only the lines the file could not take, and a second process of the same command logs to stderr with a `log file in use` warning. Without the section nothing changes.
