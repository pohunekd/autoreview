---
kind: changed
---
Discovery refuses a pull request whose base branch name GitHub allows but autoreview does not support (outside 1 to 255 ASCII letters, digits, `.`, `_`, `/` and `-`, or containing `..`) with its own code, `base_branch_unsupported`, instead of `github_response_invalid`; `candidates` lists it as rejected with that code, and a pull request already reported under the old code is reported once more. A stored review whose base SHA is not 40 lowercase hex digits, or whose pull request URL does not name its repository and number on the GitHub host, now fails with `review_input_invalid` before it builds a request; discovery refused such pull requests already, so reviews of GitHub pull requests are unchanged.
