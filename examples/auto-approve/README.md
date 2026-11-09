# auto-approve

A low-risk website repository where a clean review approves without a person.

- **Repository:** `pohunekd/pohunek.dev` (`ai:review` label)
- **Reviewer:** one Sonnet reviewer; incremental reviewers equal the full reviewers
- **Verifier:** Sonnet re-checks blocking findings; a demoted critical or high
  finding, or two demoted findings, turns the approval into a wait for a person
- **Outcome:** blocking findings request changes; a clean review is approved
- **Authors:** write-permission authors only; other authors are skipped, so an
  unknown author cannot obtain an automatic approval

Read the [guide](../../docs/knowledge/guides/auto-approve.md) for the
walk-through.

## Use it

```sh
autoreview init --dir /etc/autoreview --login <login> --pohunek-host <host> \
  --example auto-approve
autoreview --config /etc/autoreview/config.yaml validate
```

`init` writes the config, this policy and its prompts, and never overwrites a
file. To copy them by hand instead:

```sh
mkdir -p /etc/autoreview/prompts
cp examples/config.yaml /etc/autoreview/config.yaml
cp examples/auto-approve/policy.yaml /etc/autoreview/policy.yaml
cp prompts/generic.md prompts/verifier.md /etc/autoreview/prompts/
autoreview --config /etc/autoreview/config.yaml validate
```

Set the keys that [use one](../README.md#use-one) names first.

Install the `autoreview-sonnet` profile on the review host. Approvals are posted
from the account of the credential that serves `pohunekd/pohunek.dev`, so that
credential's `login` must be the account that should approve, and its token variable
must hold that account's token (see the [credentials](../../docs/configuration.md#credentials)).

Files: [policy.yaml](policy.yaml), shared [config.yaml](../config.yaml).
