# AGENTS.md

Canonical entry point for every agent working in this repository. Keep it
short: it routes to the scoped guides instead of repeating them. When a command,
convention, or boundary changes, update the guide that owns it in the same PR.

All repository text, code, comments, commit messages, issues, and PRs are in
English. Priorities: security > correctness > simplicity > speed of delivery.

## About

autoreview is a small Go service that reviews GitHub pull requests with coding
agents (Claude Code, Codex) running as read-only reviewers in
[pohunek](https://github.com/zajca/pohunek) sessions on the operator's own
machines. It is policy-driven, publishes one GitHub review per pull request, and
keeps its state in SQLite. The public design is in [README.md](README.md) and
[docs/features.md](docs/features.md); the concepts are in
[docs/knowledge/concepts/](docs/knowledge/concepts/).

The repository is in planning: it holds documentation, examples and prompts,
and no engine code yet. The engine is extracted in milestone M1. The app is
expected to change quickly. Optimize for small, fast, reversible changes: little
ceremony, few tests, a fast CI.

## Instruction routes

Read a guide when its trigger applies.

- **Every task:** [.agents/instructions/workflow.md](.agents/instructions/workflow.md)
  for autonomy, issues, branches, PRs, merge, and the rule to file problems
  instead of fixing them.
- **Any code change:** [.agents/instructions/coding.md](.agents/instructions/coding.md)
  for Go, the Python adapters, the pohunek boundary, configuration, security,
  and comment rules.
- **Any user-visible change:** [.agents/instructions/changelog.md](.agents/instructions/changelog.md)
  for the changelog fragment in `changelog.d/` and when `no-changelog` applies.
- **Before any commit or PR:** [.agents/instructions/verification.md](.agents/instructions/verification.md)
  for the gate set, the testing policy, CI, and evidence.
- **Public documentation:** `docs/` is user documentation checked by
  `scripts/check-docs`. A change in behaviour updates the matching page in the
  same PR.
- **Architecture or product decisions:** the public design pages named above
  are authoritative; revise them explicitly in the same PR when a decision
  changes.

## Skills

Canonical skills live in `.agents/skills/`; `.claude/skills/` contains only
links to them. Edit only `.agents/skills/`.

- [github-workflow](.agents/skills/github-workflow/SKILL.md): issues, epics,
  milestones, project status, and writing reproducible issues.
- [deliver-issue](.agents/skills/deliver-issue/SKILL.md): take one issue from
  intake to a merged, verified, closed result without asking.
- [plan-epic](.agents/skills/plan-epic/SKILL.md): split an epic or milestone
  into ready, well-formed issues.
- [gates](.agents/skills/gates/SKILL.md): run the local gate set and report it
  honestly.

## Commands

```bash
scripts/gates                  # every gate: docs, then Go once go.mod exists
scripts/gates docs             # documentation and example checks only
scripts/gates go               # Go gates only (skipped with a reason until go.mod exists)
python3 -I scripts/check-docs  # the documentation checker alone
```

Never read `.env*`, key, or credential files, and never print a secret value.
Never call `env` or `printenv` (the owner's agent settings stop on them):
check a variable with `test -n "$VAR"` and list names with `compgen -e`.
