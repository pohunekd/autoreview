---
kind: added
---
`github.credentials` accepts several credentials and `match` patterns other than `*`: each repository is served by the first credential whose `match` covers it. A `match` entry that an earlier one covers fails as `configuration_invalid`, a policy repository without a credential as `credential_not_covered`, and a credential that serves none as `credential_unused`. Commands other than `validate` still refuse several credentials until they use a token per repository.
