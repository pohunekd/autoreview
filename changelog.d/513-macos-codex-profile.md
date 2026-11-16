---
kind: changed
---
`autoreview profiles install --agent codex` on a host other than Linux, such as a Mac review host, fails with `profile_agent_unsupported` before it calls pohunek or writes a file, because the Codex adapter needs Linux `PR_SET_CHILD_SUBREAPER` to end the processes that Codex leaves. Claude reviewers install as before, and a policy may still name Codex reviewers on Linux hosts. ([#513](https://github.com/pohunekd/autoreview/issues/513))
