---
kind: changed
---
The verifier prompt `prompts/verifier.md` checks that the trusted head commit is in the repository (`git cat-file -e`) and fetches `pull/<number>/head` only when it is missing, then reads the change by the head SHA in every case. A review host that pins the head at the session start (pohunek 0.43.0 or later) no longer makes the verifier fetch it again; copy the new prompt to each policy directory that holds its own copy.
