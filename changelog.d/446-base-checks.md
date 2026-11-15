---
kind: changed
---
Discovery refuses a pull request whose base branch name GitHub allows but autoreview does not support (outside 1 to 255 ASCII letters, digits, `.`, `_`, `/` and `-`, or containing `..`) with its own code, `base_branch_unsupported`, instead of `github_response_invalid`; `candidates` lists it as rejected with that code, and a pull request already reported under the old code is reported once more.
