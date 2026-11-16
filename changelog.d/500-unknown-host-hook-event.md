---
kind: security
---
Under `pohunek.head_checkout_host_hooks: refuse`, a `checkout: head` review is also refused with `review_host_hooks_refused` when the review host's `host_hooks` reports anything but `false` for a hook event that autoreview does not know, so a host script for an event that a later pohunek adds cannot run in the pull request's worktree unchecked. Only `pre_create` and `post_remove`, which run in the project repository, are still ignored.
