---
type: Guide
id: guide/text-guard
title: Enable the text guard
description: Turn on the prompt-injection text guard of a rule step by step - a lexicon, shadow mode with on_suspect log, a synthetic injection corpus and a false-positive corpus measured with autoreview guard scan, the targets, and when to move a rule to await or comment.
source_kind: manual
intents: [setup, help]
---

# Enable the text guard

> **Status: after 0.3.0.** This page describes autoreview on `main` after the
> 0.3.0 release. A part that is not available names its open issue.

The text guard of a rule ([`guard`](../../policy.md#guard)) scans the text of a
pull request for instructions hidden from a human reader before the reviewers
run. On a rule that approves clean reviews by itself, a suspect pull request
then waits for a person. A guard that flags too much makes every approval wait,
so a rule turns it on in steps: a lexicon, shadow mode, a measurement on two
corpora, and only then an outcome that changes the review.

The guard runs on the autoreview host. It sends no text to a model or a
service; the extra GitHub read calls fetch the texts of guarded rules only. Its
messages and its line in the review body carry signal codes, never text.

## Write a lexicon

Copy the starter lexicon and add the phrases you expect in an injection aimed
at your reviewers. The format is in [configuration](../../configuration.md#guards).

```sh
install -m 0644 examples/guard-lexicon.txt /etc/autoreview/guard-lexicon.txt
```

Add the [`guards.text`](../../configuration.md#guards) section to
`config.yaml` and run `autoreview validate`, which reads and checks the
lexicon:

```yaml
guards:
  text:
    max_input_bytes: 1048576
    lexicon_file: /etc/autoreview/guard-lexicon.txt
```

A phrase adds its weight once per pull request, whatever the number of
matches. Each hidden-text signal weighs 100, a chat-template token 30 and a
bidi control on an added code line 40. A lexicon weight is from 1 to 100, so a
phrase can make a pull request suspect alone only when its weight reaches the
threshold.

## Run it in shadow mode

Give the rule a guard with `on_suspect: log`. Nothing about its reviews
changes: each suspect pull request sends one `guard_suspect` message at level
`action` with its codes and score, and the result is stored with the review.

```yaml
repositories:
  - match: acme/widget
    # trigger, pipeline, on_findings, on_clean and authors as before
    guard:
      fields: [title, body, commit_message, comment, review_thread, patch]
      threshold: 100
      on_suspect: log
```

Restart `serve` after the change. Keep the rule in shadow mode while you
measure, and read each `guard_suspect` message: a hidden instruction in a real
pull request is worth a look at the pull request on GitHub; a false positive is
worth a note of its signal code.

## Check the synthetic injection corpus

`testdata/guard/injection-corpus.jsonl` holds twelve synthetic pull requests of
`acme/widget`, one per technique, written with JSON escapes so that the file
shows every invisible character. Scan it with your lexicon and the rule's
values:

```sh
autoreview guard scan --lexicon /etc/autoreview/guard-lexicon.txt \
  --max-input-bytes 1048576 --threshold 100 \
  --fields title,body,commit_message,comment,review_thread,patch \
  testdata/guard/injection-corpus.jsonl
```

With the starter lexicon and these values the summary is (`signals` left out):

```json
{"pull_requests": 12, "flagged": 7, "flagged_percent": 58.33, "hidden_text": 6, "truncated": 1}
```

| Numbers | What they model | Suspect at 100 |
|---|---|---|
| 1, 2 | Clean text: code, emoji, a subdivision flag, right-to-left prose, a `<details>` log | No |
| 3, 4, 7 | Tag characters in the body, a zero-width run in a commit message, a right-to-left override in a review thread | Yes |
| 5, 6, 8 | An HTML comment, a `<details>` block and image alt text that instruct the reviewer | Yes |
| 9, 10, 11 | A chat-template token, visible lexicon phrases, bidi isolates on an added line | No: 30, 65 and 40 stay below 100 |
| 12 | Clean text that the fetch left out in part (`truncated`) | Yes, it fails closed |

Every hidden-text line must be suspect with your values. When one is not, the
threshold is above 100 or `fields` leaves out the field it uses.

## Fetch a false-positive corpus

`scripts/guard-corpus` writes a corpus of real, closed pull requests: your own
repositories, where the guard will run, and a few busy repositories whose pull
requests discuss injections, such as security and AI tooling projects, where
false positives are likely. For example, the last 200 of yours and 100 of
another project:

```sh
python3 -I scripts/guard-corpus --gh-binary /usr/bin/gh --count 200 \
  --page-limit 20 --output own.jsonl acme/widget
python3 -I scripts/guard-corpus --gh-binary /usr/bin/gh --count 100 \
  --page-limit 20 --output tooling.jsonl example-org/agent-tools
cat own.jsonl tooling.jsonl > corpus.jsonl
```

The script reads through `gh api`, with the login of that `gh`: the title, body,
commit messages, comments, review bodies, review thread comments and file
patches of each pull request. It sends them nowhere. It writes a temporary
file `.<output>.<random>.partial` next to the output file and links it to the
output path only after the last pull request, so a corpus file exists only
after a complete fetch: a run that fails, or that you stop with Ctrl-C,
`kill`, `timeout` or by closing the terminal, leaves no file. The output file
must not exist yet, and one that appears during the run is not overwritten.
Only a run killed with `kill -9` can leave the temporary file behind, never
the output file; delete it. It sets `truncated` by the rules of a review's
read, with `--page-limit` as your `schedule.search_page_limit`, and also for
250 commits (the most the REST API lists) and for review comments past the
page limit, where a review counts threads. It keeps the comments of the
reviewing account, which a review leaves out. So the corpus can only flag more
than the reviews would. A corpus holds the full text of public pull requests: keep it out of
the repository.

## Pick the fields and the threshold

Scan the corpus with the values you want the rule to use, then read the
summary:

- `hidden_text`: the pull requests with a hidden-text signal. Open each one
  (the line names repository and number) and decide whether it is a real
  injection or a false positive.
- `truncated`: the pull requests whose text was left out. They are suspect
  whatever the score. With `patch`, every pull request that changes a large
  generated file (a lockfile, a vendored bundle) counts here, because GitHub
  leaves out its patch. If they dominate, scan without `patch` and compare.
- `flagged_percent`: the share of suspect pull requests.
- `signals`: the pull requests per code, to see which signal or lexicon phrase
  flags most.

A threshold of 100 makes any one hidden-text signal suspect. A higher threshold
needs a combination, such as hidden text plus a lexicon phrase, and lets more
attacks through. A larger `max_input_bytes` truncates fewer pull requests, at a
longer scan time.

## The targets

Move a rule out of shadow mode only when, with its exact values:

1. every hidden-text pull request of the synthetic corpus is suspect;
2. the false-positive corpus has no hidden-text false positive (`hidden_text`
   counts only real injections, normally 0);
3. under 2 % of the false-positive corpus is flagged (`flagged_percent` below
   2), truncated pull requests included;
4. the `guard_suspect` messages of shadow mode match that rate and hold no
   hidden-text false positive.

## Move from log to await or comment

- `await` fits a rule with `on_clean: approve`. A suspect clean review waits
  for a person, who approves the exact head as with any other wait, see
  [human approval](human-approval.md). Reviews with blocking findings are
  published as usual.
- `comment` fits a rule that reviews untrusted authors: a suspect pull request
  of an untrusted author gets a comment where it would get a request for
  changes, so a hidden instruction cannot make the account block it. It does
  not hold an approval.
- Keep `log` on a rule where neither applies, for example `on_clean: await`,
  which already waits for every clean review.

Change `on_suspect`, run `autoreview validate` and restart `serve`. The policy
digest changes with every `guard` change. Repeat the measurement when you
change the lexicon, the fields or the threshold.

## Related pages

- [Policy: guard](../../policy.md#guard)
- [Configuration: guards](../../configuration.md#guards)
- [CLI: guard scan](../../cli.md#autoreview-guard-scan)
- [Prompt injection](../safety/prompt-injection.md)
- [Threat model](../safety/threat-model.md)
