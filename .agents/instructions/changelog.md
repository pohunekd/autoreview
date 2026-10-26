# Changelog fragments

Every user-visible change adds a fragment under `changelog.d/`. Fragments are
folded into `CHANGELOG.md` when a release is prepared, and the release body
shows that section. The contract is shared by the Pohunek ecosystem; this file
is the repository's copy of its rules. The reference implementation is the
changelog tool in
[pohunek-companion](https://github.com/pohunekd/pohunek-companion/tree/main/crates/changelog).

## When a fragment is required

- Required: anything a user, operator or packager sees or relies on (behaviour,
  CLI output, configuration, installed files, supported platforms, a fixed bug
  users hit).
- Not required, label the pull request `no-changelog`: CI, tests, refactors,
  internal docs, agent instructions, release preparation.

## Writing a fragment

Create `changelog.d/<N>-<slug>.md`, where `N` is the issue (or pull request)
number without leading zeros and `slug` is lowercase words joined by hyphens.

```markdown
---
kind: fixed
---
Pull requests whose only matching rule is suspended are no longer reviewed
until the credential is replaced.
```

- Header keys: `kind` (required): `added`, `changed`, `deprecated`, `removed`,
  `fixed` or `security`. `breaking: true` marks a breaking change (optional,
  default `false`). No other keys.
- Body: one paragraph of one to three sentences, present tense, written for the
  user: what changes for them. Markdown inline syntax only; no headings, no
  blank lines inside the paragraph.
- Keep the file UTF-8 with LF line endings.

## Commands

The changelog tool is filed as M1 work (`scripts/changelog`). Its CI check starts
once the script exists; until then the pull request template still asks for the
fragment, and reviewers hold the rule by hand. Its commands will be:

```bash
scripts/changelog check                       # validate every fragment (CI runs it)
scripts/changelog notes X.Y.Z                 # the release block for a folded version
scripts/changelog fold X.Y.Z [--date YYYY-MM-DD]   # release preparation only
```

`fold` is run only by release preparation (M4). Feature pull requests never fold
and never edit `CHANGELOG.md`.
