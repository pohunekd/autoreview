---
kind: security
---
Model text in a published review can no longer reference an issue, pull request or discussion of another repository, which GitHub records in that repository's timeline, or a commit there: `owner/repo#12`, `owner/repo/issues/12`, GitHub URLs to them and `owner/repo@sha` get a zero-width space, while references to the reviewed repository (`#12`) still link. A link reference definition in model text is escaped, so it cannot turn engine text such as a login into a link.
