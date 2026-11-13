---
kind: changed
---
Every GitHub read takes the login for review requests, own pull requests and the published-review lookup from the credential that serves the repository. The review queue groups its entries by account when several credentials are configured, and leaves out, without a read, the notes hand-offs of a credential whose search failed and of repositories that no credential serves. Commands other than `validate` still refuse several credentials, until the credential of each review is stored (#18).
