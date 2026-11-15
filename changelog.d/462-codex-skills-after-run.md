---
kind: fixed
---
Codex reviewer runs on a `checkout: head` pull request now compare the repository skill folders with the trusted base again after the run, so a skill that a process of the head (for example a pohunek hook) left on disk during the run fails the review with `review_head_skills_refused` instead of being published. The change reaches a review host only when its profiles are installed again with `autoreview profiles install`.
