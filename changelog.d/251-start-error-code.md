---
kind: changed
---
The log line `reviewer session start failed` names the error code that pohunek reports for the failed start as its `diagnostic`, for example `session_input_not_ready`, instead of an allowlisted class such as `unclassified_cli_failure`. A code that does not match `^[a-z][a-z0-9_]{0,63}$` is not logged, and a start failure without a code keeps its class.
