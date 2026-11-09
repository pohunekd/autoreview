---
kind: changed
breaking: true
---
Every repository rule of the policy now needs an `authors` section, and a policy without one is refused at load with `authors is required`. To keep reviewing every author as before, forks included, add `authors: {trusted: any}` to each rule; logins, permissions and `untrusted` sections are validated but refused until author classes are enforced.
