---
kind: added
---
Policy rules accept an optional `guard` section (`fields`, `threshold`, `on_suspect`) and `config.yaml` an optional `guards.text` section (`max_input_bytes`, `lexicon_file`) for the prompt-injection guard. Both are validated at load, the lexicon file included, a rule with `guard` needs `guards.text`, and a `guard` changes the policy digest. A rollback to an earlier release must remove both sections again, because earlier releases refuse them as unknown.
