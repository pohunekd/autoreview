---
type: Guide
id: guide/eval-suite
title: Regression-test a review configuration
description: Check a change to a prompt, model, reviewer profile or policy against known pull requests with autoreview eval - the case files and fixture branches, turning a missed defect or a judged finding into a case with eval add, a config file of its own with a read-only credential, recall and false blocking thresholds, repeats, cost bounds, the runner precondition and the limits of the run.
source_kind: manual
intents: [setup, help]
---

# Regression-test a review configuration

> **Status: after 0.6.0.** This page describes autoreview on `main` after the
> 0.6.0 release. A part that is not available names its open issue.

An eval reviews known pull requests with the real reviewers and verifier of one
pipeline, publishes nothing, and counts how many of the expected blocking
findings it caught and how often it blocked a clean change. Run it before a
prompt, model, effort, reviewer profile or policy change reaches production
reviews. Reviewers are not deterministic and every trial costs real model calls,
so start with a few cases, `--repeat` and generous thresholds.

## Write the cases

A case is one `<id>.yaml` file; [`eval validate`](../../cli.md#autoreview-eval-validate)
checks a directory offline. It names the repository, the pull request number, the
base branch, the base and head SHAs, and either the blocking findings a review
must report (a path, a line range of at most 200 lines on the right side of the
head, and a description) or `expect: clean`. The optional `pipelines` list keeps a
case out of the other pipelines. [`examples/eval`](../../../examples/eval/) holds a
defect case and a clean case of this repository; each file names the review that
proves it.

A finding matches an expected one when it blocks and has the same path and a
reported line inside the range. The line is the reviewer's own claim, so a
finding that anchoring moved into the range earns nothing; the report lists it as
`moved_into_range`.

The session fetches the head from `head_ref`, `refs/pull/<number>/head` unless
the case names a branch, and the fetched ref must peel to `head_sha`. A defect
that a later commit fixed is no longer what the pull request ref points at, so
give it a fixture branch at the commit that had the defect:

```
git push origin 8615dad683e4775931552a5a72d78f314c70c714:refs/heads/eval-cases/check-docs-text-block
```

Protect `eval-cases/**` with a ruleset that blocks deletion and force pushes. A
branch that moved fails its trials at the fetch and never scores another commit.
A `head_ref` branch under `session.branch_prefix` or `eval.session_branch_prefix`
is refused when the run starts.

## Turn a missed defect into a case

A defect that reached the default branch past a review is the best case to keep.
Take the base and head SHAs of the pull request that the review saw and the commit
that fixed the defect, and run [`eval add`](../../cli.md#autoreview-eval-add) with
the eval config:

```
autoreview --config eval-config.yaml eval add --cases examples/eval/cases \
  --from-pr acme/widgets#214 --fix <fix-sha> --id widgets-cache-race \
  --base-sha <base-sha> --head-sha <head-sha> --base-branch main
```

It writes `widgets-cache-race.yaml` and prints it. Read the printed ranges and
`skipped` before committing the file:

- The expected lines are the ones the fix changed in the file as it was before the
  fix, so the case is only written when the file is the same at the reviewed head.
  A file that moved needs `--lines PATH:FIRST-LAST`.
- A file qualifies only if the pull request has an added line in it: a reviewer
  cannot block on a file it did not change, so `skipped` lists the fix's other
  files. Pass `--path` for each file when several qualify.
- A suite holds one case per pull request head, so a second miss on the same head
  is refused with the name of the file to edit by hand.
- When `refs/pull/<number>/head` moved past the reviewed head, pass `--head-branch`
  and run the two `git` commands that the output prints, with your own credentials;
  until the branch exists, the case fails at the fetch.

Then run `eval validate` on the directory and `eval run` on the new case. With
`--store` (below) the base and head come from the stored review, and
`--base-sha`, `--head-sha` and `--base-branch` can be left out.

## Turn a judged finding into a case

A finding that was wrong, or right but never acted on, is worth keeping too. Find
its key in `findings list`, then let `eval add` read the store, read-only:

```
autoreview findings list --repo acme/widgets --since 2026-11-01 \
  | jq -r 'select(.outcome == "refuted") | "\(.repository)#\(.number):\(.finding_key)"'
autoreview --config eval-config.yaml eval add --cases examples/eval/cases \
  --from-finding acme/widgets#214:0123456789ab:O003 --verdict wrong \
  --id widgets-false-lock --store /var/lib/autoreview/autoreview.db
```

- `--verdict wrong` writes a clean case for the whole head, so it is refused while
  another blocking finding stood at that head, one carried from an earlier push
  included unless it was fixed by then; write that case by hand.
- `--verdict right` needs your own `--lines PATH:FIRST-LAST` and `--description`:
  the stored line is where anchoring put the finding, not where the defect is. As
  for a fix, the path needs an added line in the pull request.
- `--store` is the production database, opened read-only while `serve` runs: no
  database file or directory is created and no engine or discovery lock is
  taken. Without `serve`, SQLite leaves `-wal` and `-shm` files beside it. Never
  use SQLite's `immutable` mode on it, and as any user other than the service
  user, point `--store` at a `backup-db` copy.

## Set up the run

Run an eval from a config file of its own, on the review host:

- The `eval` section ([configuration](../../configuration.md#eval)) names a
  credential whose token only reads (contents and pull requests), so GitHub itself
  refuses a write if autoreview ever tried one. The credential check of a
  configuration refuses a credential that no policy rule serves, so the eval file
  holds a policy that covers the case repositories and that credential.
- Set `session.name_prefix` and `session.branch_prefix` of the eval file to the
  production values, and choose eval prefixes that differ from them. The check
  compares the eval prefixes with the `session` section of the same file only, so
  with other values in the eval file it would pass while a production session could
  still be taken for an eval one, or `prune-branches` could delete an eval branch.
- The review host needs a pohunek project for each case repository, as for any
  review. `eval.max_trials` and `eval.max_sessions` bound the cost; a run past
  them is refused or stops its sessions.
- `--git-binary` is the absolute path of the git program that deletes the review
  branches the run started, after it removed its sessions.

The run uses a store in a private temporary directory, takes no lock of `serve`
and can run beside it. Its integrations answer every read from GitHub and pohunek
and refuse or ignore every write; a test pins that no publication, notification
or lookup of a published review is made.

## Read the result

The report gives, per case and per repeat, the findings, which expected findings
were caught, and why a miss happened (`not_reported`, `reported_as_note`,
`skipped_by_rules`, `note_after_anchoring`, `demoted_by_verifier`). The totals give
the recall over the defect cases and the false blocking rate over the clean cases,
before and after the verifier, and the report lists the branches deleted and the
branches left on their host. Use `--min-recall` and `--max-false-blocking` to gate
on the rates after verification; a threshold needs a case of its class, or the run
does not start. Exit code 2 is a regression, and 3 is a trial that could not be
measured, a threshold without a completed trial, or a cancelled run; see
[`eval run`](../../cli.md#autoreview-eval-run). A case that passes in some repeats
and not in others is `flaky`.

## Limits

- The range of changed files comes from GitHub's compare endpoint, which lists at
  most 300 files, on its first page only. A comparison that reaches the limit, an
  answer without files, or a changed file with added lines and no patch fails the
  trial instead of scoring with fewer lines.
- A pipeline with `checkout: head` cannot be evaluated and is refused at the start.
- With `checkout: base`, a trial's worktree starts at today's tip of the base
  branch, not at the case's `base_sha`. The reviewer reads the change and the head
  code through git, but repository guidance and the surrounding code come from the
  current base branch, so an old case can score differently as that branch moves.
- A session that the review host keeps after a trial is listed in `sessions_left`
  of the report; remove it by hand.
- A branch on a review host that is not the machine of the run, or that a session,
  worktree or hook keeps, is left and listed in `branches_left`.
- A killed eval run (SIGKILL) leaves its sessions and branches on the review host
  until they are deleted by hand.
- Decisions (approve, request changes) are not evaluated: the rule of the pipeline
  is run with comment outcomes, no human review signals and no text guard.

## Run it in CI

[`examples/eval/workflow.yml`](../../../examples/eval/workflow.yml) is a template
for a self-hosted runner, because reviewers are pohunek sessions. It runs on
dispatch, on a schedule and on a push that edits prompts or policy, with read-only
permissions, and its actions are pinned by commit SHA.

The runner is the security boundary, not the workflow file: on a public
repository a pull request from a fork can add a workflow that asks for the same
runner label. Put the runner in a runner group restricted to this repository and
to this workflow file on the default branch, and require approval for runs from
outside collaborators. The eval token lives in the `autoreview-eval` environment
as an environment secret. A `pull_request` trigger is shown commented out; enable
it only with that environment (required reviewers) and the job condition for the
same repository, and never with `pull_request_target`. The `policy_file` of the
eval config must point at the policy of the checked-out workspace, or a push tests
the installed policy instead.
