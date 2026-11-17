# Eval suite example

Two cases of the public repository `pohunekd/autoreview` and a workflow template.
`cases/` holds one defect case and one clean case; `workflow.yml` runs the suite on a
self-hosted runner. The guide is
[Regression-test a review configuration](../../docs/knowledge/guides/eval-suite.md).

```
autoreview eval validate --cases examples/eval/cases
autoreview --config eval-config.yaml eval run --cases examples/eval/cases --pipeline default \
  --git-binary /usr/bin/git
```

The comment at the top of each case file names the review that proves it. The defect case
needs the branch `eval-cases/check-docs-text-block` of the repository.
