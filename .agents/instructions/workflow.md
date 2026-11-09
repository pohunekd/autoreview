# Workflow and autonomy

All work is planned, tracked, and recorded in GitHub: issues, native
sub-issues for epics, milestones for phases, and the
[autoreview project](https://github.com/orgs/pohunekd/projects/3) for status.
`.github/agent-workflow.json` holds the identifiers; the
[github-workflow skill](../skills/github-workflow/SKILL.md) holds the procedures.
Nothing important lives only in chat, local notes, or agent memory.

## Standing authorization

The owner durably authorizes agents in this repository, without asking first,
to:

- create, edit, comment on, label, link, and close issues; assign milestones;
  add items to the project and set their status;
- create branches, commit, push branches, and open PRs;
- merge their own PR once the merge bar below holds, and delete its branch.

Not covered (needs an explicit owner request): releases and tags, pushes or
force-pushes to `main`, repository settings, rulesets, secrets, Actions
permissions, deleting issues, and closing another agent's open work.

## Never block on the owner

Do not ask the owner questions during work. Resolve ambiguity with the most
conservative reading of the issue and the public design pages, and record it on
the issue as `Decision (autonomous): ... because ...`. A choice that really is
the owner's (product direction, scope change, a new external dependency with
licensing or security weight) gets the `needs-decision` label and a comment
that states the options and a recommendation; continue with the part that does
not depend on it or pick other ready work.

## File problems, do not fix them

The issue's definition of done is the scope. When you meet a problem outside
it (a bug, flaky behaviour, a doc error, a missing feature, a CI or tooling
defect):

- **Not blocking your issue:** file it (dedup first) as a reproducible issue
  per the github-workflow skill, label `found-during-work`, mention the issue
  you were working on, and continue. Do not fix it in the current PR, even if
  it looks like a one-liner.
- **Blocking your issue:** if the smallest fix that unblocks you is inside the
  issue's scope, make it and say so in the PR. Otherwise file it as above, mark
  your issue `blocked` with a comment linking the blocker, set the native
  dependency, and stop or move to other ready work.

Never widen a PR silently, and never move an unmet DoD item into a follow-up
to call the current issue done.

## Issue to PR

- One issue, one branch, one PR. Keep PRs small; an issue too large for one
  reviewable PR is split into sub-issues (plan-epic skill), not into stacked
  PRs.
- Branch: `<issue-number>-<short-slug>`. Parallel agents work in separate
  worktrees under `../autoreview-worktrees/<slug>` and never share a build
  cache concurrently.
- Commits: imperative English subject, body with the why when not obvious, no
  `Co-Authored-By` or generated-by trailers, unsigned.
- PR body: `Closes #N`, what changed and why, the gate results, and manual
  verification evidence (see verification.md). The before/after story lives
  here, never in code comments.

## Merge bar

An agent merges its PR (squash, PR title as subject, branch deleted) when all
of these hold on the current head:

1. The required `ci` check is green.
2. A self-review by an independent reviewer (a fresh subagent or Codex that did
   not write the change) found no unresolved correctness or security problem;
   each finding is fixed, filed as an issue, or rejected with evidence in a PR
   comment.
3. Every DoD item is met with evidence in the PR.
4. Right before the merge, the branch is rebased onto the current `origin/main`
   and `scripts/gates` passed on the rebased tree. The ruleset does not require
   an up-to-date branch, so two PRs that are each green can still break `main`
   together; this step catches that.

After merging, confirm the merge commit is on `origin/main` and its `main` CI
run is green, then close the issue and set the project status to Done. A red
`main` is fixed first, before any other work.

## Releases

Releases and tags are owner-only (see "Not covered" above); an agent prepares
the release pull request only when the owner asks. `.github/workflows/release.yml`
builds the archives from a pushed tag `v<version>` and creates a draft release.
It never creates a tag and never publishes.

1. On a branch `release-X.Y.Z`, set `version = "X.Y.Z"` under
   `[workspace.package]` in `Cargo.toml`, then run `cargo update --workspace`.
   It updates only the workspace crates in `Cargo.lock`; `--locked` fails
   without it.
2. Run `scripts/changelog fold X.Y.Z`. It moves every fragment into a new
   section of `CHANGELOG.md` and deletes the fragments. Run `scripts/gates`.
3. Open the pull request with the label `no-changelog` and merge it when `ci`
   is green.
4. Tag the merge commit on `main` and push the tag:
   `git tag -a vX.Y.Z -m "autoreview X.Y.Z" <merge-commit>`, then
   `git push origin vX.Y.Z`. The workflow refuses a tag that is not `v` plus
   the workspace version, and a version without a section in `CHANGELOG.md`.
5. Wait for the `Release` run. It builds one archive per target, attests the
   archives and their `.sha256` files, and creates a draft release whose body
   is the `CHANGELOG.md` section. Check the draft (two archives, two `.sha256`
   files, the notes), then publish it.

A failed run deletes the draft it created and never touches the tag; fix the
cause and re-run the failed jobs. A leftover draft of the tag blocks the run:
delete it first. A manual run of the `Release` workflow is a dry run: it builds
the archives of the selected branch and uploads them as workflow artifacts
only.

## Handoff

When you stop before Done, leave one comment on the issue with the branch, head
SHA, what is finished, what remains, gate results, and blockers. The next agent
must be able to continue from the issue alone.
