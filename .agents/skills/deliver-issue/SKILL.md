---
name: deliver-issue
description: >-
  Take one autoreview issue from intake to a merged, verified, closed result
  without asking the owner: claim it, implement in a worktree, pass the gates,
  get an independent self-review, open the PR, wait for CI, squash-merge, verify
  main, and file every problem met on the way. Use when handed an issue ("deliver
  #N", "dotáhni #N") or when picking the next ready issue.
---

# deliver-issue

The issue's DoD is the contract. Follow `.agents/instructions/workflow.md`
(autonomy, file-don't-fix, merge bar) and the github-workflow skill for every
GitHub write. Never end a turn waiting on the owner; wait for CI with a
background watch so its completion resumes the work.

## 1. Intake

1. Read the issue body and all comments. If it is assigned to someone else,
   `In Progress`, `blocked`, or `needs-decision`, pick another issue.
2. If the DoD is missing or not checkable, rewrite the body into the template
   shape first and record `Decision (autonomous): ...` for anything you had to
   interpret. If the issue is too large for one PR (soft limit in
   `.github/agent-workflow.json`), split it with the plan-epic skill and deliver
   the first sub-issue instead.
3. Assign yourself and set the project status to `In Progress`.

## 2. Implement

1. Create the branch `<n>-<slug>` from fresh `origin/main` in a worktree:
   `git worktree add ../autoreview-worktrees/<n>-<slug> -b <n>-<slug> origin/main`.
2. Read `.agents/instructions/coding.md` and the public design pages the issue
   touches (`docs/features.md`, `docs/knowledge/concepts/`, `docs/configuration.md`,
   `docs/policy.md` as relevant).
3. Implement the whole DoD: no stubs, placeholders, or TODOs standing in for
   specified behaviour. Write tests only where the testing policy asks.
4. Anything outside the DoD that you notice: file it, do not fix it.
5. Parallel subagents are fine for independent parts; each owns separate
   files and gets the context it needs (issue, files, constraints, report
   format).
6. Update the public documentation that the change makes wrong, in the same PR.

## 3. Verify

1. `scripts/gates` on the final revision; every gate passes or is reported as
   skipped with its reason.
2. Run the manual check named in the DoD against a real pohunek daemon and the
   scratch repository (verification.md); record what you observed in words.

## 4. Self-review

Give the full diff (`git diff origin/main...HEAD`) and the issue to an
independent reviewer that did not write the code (a fresh subagent, or Codex
through the second-opinion skill). Ask for correctness, security, error
handling, and DoD coverage findings with `path:line`. For each finding: fix it,
file it (out of scope), or reject it with evidence. Re-run the gates when code
changed.

## 5. PR and CI

1. Commit (imperative subject, no trailers), push the branch, and open the PR
   with the template: `Closes #N`, DoD table, gate results, manual evidence,
   self-review outcome, filed issues.
2. Watch the PR checks (`gh pr checks <pr> --watch` in the background). On
   failure read the log (`gh run view <run> --log-failed`), fix the cause, and
   push. A failure unrelated to your diff is filed as an issue; rerun the job
   once, and if it fails again, mark your issue `blocked` on it.
3. If `main` moved, update the branch (`gh pr update-branch <pr>` or rebase)
   and wait for CI again.

## 6. Merge and close

1. When the merge bar holds: `gh pr merge <pr> --squash --delete-branch`.
2. `git fetch` and confirm the squash commit is on `origin/main`; watch the
   `main` CI run for it. If it is red, fix `main` before anything else.
3. Confirm the issue closed and its project status is `Done`; add a final
   comment with the merged PR, DoD verdicts, and filed follow-ups.
4. Remove the worktree and local branch.

## Stop conditions

Stop only when the issue does not exist or has no actionable scope, a
credential or permission is missing, or a blocker you filed prevents all
progress. Record the stop as a comment with the handoff details, then report.
