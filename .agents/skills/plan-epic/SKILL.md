---
name: plan-epic
description: >-
  Split an autoreview epic, milestone, or oversized issue into small, ready
  issues with checkable DoD, linked as native sub-issues with dependencies, in
  the right milestone and the project. Use when an epic has no sub-issues yet,
  when a milestone needs its next issues, or when an issue is too large for one
  PR.
---

# plan-epic

1. Read the epic (body and comments), the public design pages it relates to
   (`docs/features.md`, `docs/knowledge/concepts/`, `README.md` rough plan), and
   the current code it touches. Verify each candidate piece of work is not
   already done or filed (search open and closed issues).
2. Cut the scope into issues that each fit one PR (soft limit in
   `.github/agent-workflow.json`) and deliver something runnable on their own.
   Order them by dependency; prefer a thin end-to-end slice first over
   layer-by-layer work.
3. Write each issue per the github-workflow skill (Feature or Task template,
   stable D items, entry points, verification). Put open questions that only
   the owner can answer in a separate `needs-decision` issue with options and
   a recommendation, instead of guessing them into a DoD.
4. Create the issues in the epic's milestone, add them to the project as
   `Todo`, link each as a native sub-issue of the epic, and set native
   `blocked_by` dependencies. Read every link back.
5. Comment on the epic with the ordered list of sub-issues and the reasoning
   for the cut.

Do not plan beyond what the current design supports: work that depends on an
unreleased pohunek feature is filed as `blocked` on the upstream issue (link it
by URL).
