---
name: github-workflow
description: >-
  Procedures for autoreview work in GitHub: find or file the issue before work
  starts, write reproducible bug reports and checkable feature/task issues, use
  issue types, labels, epics with native sub-issues, milestones and the project
  status, file problems met during other work, and close issues only on verified
  landing. Use whenever an agent starts work, files or edits an issue, records
  progress, or closes work.
---

# github-workflow

Read `.github/agent-workflow.json` once per session before the first write: it
names the repository, the project, the status options, issue types, and labels.
Resolve project field and option IDs at run time with
`gh project field-list 3 --owner pohunekd --format json`; never hardcode them.
The standing authorization in `.agents/instructions/workflow.md` covers every
write described here: never ask whether to file or update an issue.

## Before work starts

Every piece of work has an issue. Explicit issue number from the task wins.
Otherwise search open and recently closed issues (`gh issue list --repo
pohunekd/autoreview --search "<terms>" --state all --limit 100`) and reuse a
match; ask nothing, file a new one when there is no match. Set the issue to
`In Progress` in the project and assign it to yourself (`gh issue edit N --add-assignee @me`)
so parallel agents see it is taken. Never start an issue that is assigned to
someone else or `In Progress`.

## Writing an issue

An issue must be actionable by another agent with no access to your context.
Use the matching template's sections (`.github/ISSUE_TEMPLATE/`), write the
body to a file, and create with `--body-file`.

- **Bug:** environment (autoreview commit SHA or version, pohunek version, OS),
  exact steps from a clean start, expected vs actual with the exact error text,
  and evidence (JSON log lines, command output, CI run URL). Reproduce it
  yourself before filing; if you could not, say so and label the claim
  unconfirmed in the body.
- **Feature / Task:** objective, scope and non-goals, DoD items with stable
  IDs (`D1`, `D2`, ...) that can each be checked by a command, a named test, or
  the manual check in verification.md, entry points (`path:line` or package and
  CLI names), and the verification plan.
- One problem per issue. Title states the problem or outcome, not the
  solution ("Review of a fork pull request ignores the untrusted class", not
  "Fix policy").
- No secrets, tokens, host names, private repository names, or private paths in
  any issue text. The repository is public.

Create, then set type, labels, milestone, and project:

```bash
gh issue create --repo pohunekd/autoreview --type Bug --title "$title" \
  --body-file body.md --label "area:engine" --milestone "$milestone"
gh project item-add 3 --owner pohunekd --url "$issue_url"
gh issue view "$n" --repo pohunekd/autoreview \
  --json number,issueType,labels,milestone,projectItems   # read back
```

New project items start without a status; set `Todo` (or `In Progress`) with
`gh project item-edit --project-id <project node id> --id <item id>
--field-id <Status field id> --single-select-option-id <option id>`. Take the
item ID from the issue itself, which is never stale:

```bash
gh api graphql -F n="$n" -f query='query($n:Int!){repository(owner:"pohunekd",name:"autoreview"){issue(number:$n){projectItems(first:5){nodes{id project{number} fieldValueByName(name:"Status"){... on ProjectV2ItemFieldSingleSelectValue{name}}}}}}}'
```

Closing an issue moves its item to `Done` through the project's built-in
workflow; read the status back anyway.

## Epics, sub-issues, dependencies

An epic is a Feature labelled `epic`; its scope is delivered by native
sub-issues (`gh api -X POST repos/pohunekd/autoreview/issues/<parent>/sub_issues
-F sub_issue_id=<child database id>`). A blocker is a native dependency
(`gh api -X POST repos/pohunekd/autoreview/issues/<n>/dependencies/blocked_by
-F issue_id=<blocking database id>`) plus a `Depends on #N` line in the body. A text
link alone is not the relationship; read it back after writing.

## Problems met during other work

Follow `.agents/instructions/workflow.md#file-problems-do-not-fix-them`: dedup,
file with the Bug or Task template, label `found-during-work`, put "Found
while working on #N" in the body, add it to the project as `Todo` in the most
fitting milestone, and continue your issue. Mention filed issues in your PR.

## Progress and evidence

Body: scope, decisions, DoD, plan (long-lived; edit your own sections, read the
body before rewriting it). Comments: progress, evidence (exact commands and
results), autonomous decisions, blockers, handoffs (append-only).

## Closing

Close an issue only when every DoD item is met and, for repository changes,
the PR is merged and the `main` CI run of the merge commit is green. The PR's
`Closes #N` closes it on merge; then verify it is closed and set the project
status to `Done`. Never close with unmet DoD items; never move them to a
follow-up to make the issue look done. Use only `Refs #N` for anything a PR
does not complete: any `closes/fixes/resolves #N` wording, even negated,
closes the issue on merge.

## Safe writes

Read before writing; verify every write from the API response or a follow-up
read. After an ambiguous failure (timeout), read the state back before
retrying. A permission or network error means the write did not happen: report
it as blocked, never as done.
