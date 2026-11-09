---
kind: added
---
A repository rule can now trust only some authors: `authors.trusted` lists logins (a user as `name`, an app as `name[bot]`, matched by author type), a repository permission or both, and an untrusted author, or the author of a fork pull request who is not listed, is skipped or reviewed under the `untrusted` section from the base checkout and is never approved without the user. The class is resolved when a review is dispatched, stored with it, and logged on the `review scheduled` line.
