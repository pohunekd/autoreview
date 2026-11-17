---
kind: added
---
`autoreview eval validate --cases DIR` checks the case files of an eval suite, which records known pull requests and the blocking findings a review must report, and `autoreview eval run --cases DIR --pipeline NAME --git-binary PATH` reviews them with the real reviewers and verifier without publishing anything, deletes the review branches it started, and prints the recall, the false blocking rate and the optional gate (`--min-recall`, `--max-false-blocking`, `--repeat`) as a JSON report; the optional `eval` section of `config.yaml` configures it.
