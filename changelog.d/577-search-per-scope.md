---
kind: fixed
---
Review request searches run once per repository or owner pattern, so a fine-grained token that serves public and private repositories finds the pull requests of the private ones again with gh 2.100 or newer, and a credential that mixes an owner pattern with a repository of another owner finds both.
