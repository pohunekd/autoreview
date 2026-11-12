---
type: Guide
id: guide/multihost
title: Several review hosts
description: Run reviewers on more than one pohunek host - the deployment layout of the autoreview server and the review hosts, list the hosts in priority order, register each repository on the hosts that may review it or pin it with a route override, skip a failing host, install the agent profiles on every host, reach remote hosts over an overlay network and size the concurrency.
source_kind: manual
intents: [setup, project, debug]
---

# Several review hosts

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0.

One autoreview service can run its reviewers on several pohunek hosts. Each
review starts a pohunek session on one host, so the host that runs a review
must know the repository as a pohunek project. This guide explains how
autoreview picks the host, how to register repositories, and when to pin a
repository to a host by hand.

A single host needs none of this: list it in `pohunek.hosts`, register each
repository there, and skip the rest of the guide.
[Getting started](getting-started.md) sets up such a machine with
`scripts/setup` (after 0.2.0).

## Deployment layout

A deployment over several machines has two roles. One machine can hold both.

| Machine | Runs | Holds |
|---|---|---|
| The autoreview server | `autoreview serve` and the hourly `review-queue`, `gh`, the pohunek CLI | `config.yaml`, the policy, the prompts, the store, the secrets file |
| Each review host | the pohunek daemon with its login service, Claude Code or Codex | the official runtime packages and hooks, the agent profiles, the registered repositories |

They are joined by pohunek remote hosts: the server's pohunek CLI reaches each
review host by its pohunek name over an overlay network (see
[below](#reach-a-remote-host-over-an-overlay-network)), and `pohunek.hosts`
lists those names. autoreview itself opens no connection to a review host.

`scripts/setup` (after 0.2.0) sets up one machine with both roles. On several machines its
steps split like this:

| Step of `scripts/setup` | Autoreview server | Review host |
|---|---|---|
| autoreview binary | yes | yes, for `profiles install` |
| pohunek with its login service | the CLI and the remote host setup | yes |
| Configuration (`autoreview init`), secrets file, `validate` | yes | no |
| Runtime packages and hooks | no | yes |
| Agent profiles (`autoreview profiles install`) | no | yes, as the user of its daemon |
| User service and lingering | yes | lingering, for the pohunek daemon |

On a review host the script cannot run alone, because it writes and validates
a configuration first. Install the release archives there as
[install](../../install.md#a-release-binary) and the pohunek install guide
describe, then run the commands of the steps the table gives that host; the
[getting started](getting-started.md#the-steps) page lists them. Which host
reviews a repository is the subject of the sections below, including the
choice of a host by its state.

## Hosts in the configuration

```yaml
pohunek:
  bin: /usr/local/bin/pohunek
  state_home: /var/lib/autoreview/.local/state
  runtime_dir: /run/autoreview
  timeout_seconds: 45
  hosts: [review-host-a, review-host-b]
  route_overrides: {}
```

- `hosts` lists the pohunek host names that may run reviewers, in priority
  order. One or more.
- `route_overrides` is a map from `owner/repo` to `{host, project_id}`. It is
  empty until a repository needs a pin, see below.
- `pohunek.timeout_seconds` covers one pohunek call, including the base fetch
  of a session start, so it must be longer than the base fetch of the largest
  repository.

The host names are the ones pohunek knows; `autoreview validate` confirms that
the pohunek CLI answers and reports its version. It does not prove that every
host in the list is reachable, so check each host with a manual review.

## How a host is chosen

For each review autoreview asks the hosts in `hosts` in order for their
pohunek projects and stops at the first host that has the repository:

1. Only projects whose source is allowed by the pipeline's `project_sources`
   are candidates. `manual` means a project registered with
   `pohunek project add`; `auto` means a project pohunek discovered itself.
   A pipeline that lists both accepts both.
2. A candidate matches when its origin URL names the repository. The match is
   on the origin URL, not on the directory name.
3. The first host with a match runs the review; the later hosts are not asked.
   - Two candidates on that host: the review stops with
     `project_route_ambiguous`, unless `route_overrides` names one of them.
   - No candidate on any host: the review stops with `project_route_not_found`.

The host is chosen when a review attempt is dispatched and stored with it, so
a restarted review resumes on the same host and is never moved. Changing a
registration or the order of `hosts` takes effect with the next attempt.

## Choose a host by its state

With [`pohunek.host_state`](../../configuration.md#pohunekhost_state), the hosts
are filtered and ordered by the state that the `moc.host-state` pohunek plugin
reports (idle, power, battery, free memory, local models, policy window) before
the steps above:

1. Each host's state is read once per discovery pass, with
   `pohunek host command run HOST moc.host-state host-state --timeout-ms MS`,
   where `MS` is `read_timeout_seconds` in milliseconds. A read that fails as a
   pohunek call (no answer within `pohunek.timeout_seconds`, a daemon error)
   makes the host a failed host of the pass, as a failed project list does, so
   its outage is announced. A daemon that answers with a `command_*` code (the
   plugin is not installed, an older daemon, or the plugin did not finish within
   `read_timeout_seconds`), a document that does not read as `HostCapacity`
   v1, or a timestamp more than `max_age_seconds` away from now, either way, is
   an unknown state; `unknown_state` decides.
2. A host on battery (`on_battery: skip`, or below `min_battery_percent`), with
   less free memory than `min_free_memory_mb`, without a model of
   `required_models`, or outside its window (`outside_window: skip`) is held
   back.
3. With `order: idle_first` the idle hosts come first; within each group the
   order of `hosts` stays.

The allowed hosts are asked for their projects first. Only when none of them has
the repository are the held-back hosts asked: when one of them has it, the
review waits for a review host with `host_state_ineligible`, without a
`review_stopped` message, and the review queue lists it as `AI review waits for
a review host: host_state_ineligible`. It starts on the first pass in which the
state of a host that has it allows it. When no host has the repository, the
review fails with `project_route_not_found`, as without the section. A
repository pinned by `route_overrides` skips these rules. Install the plugin on
every review host before you add the section: a host without it has an unknown
state.

## When a host fails

A host that does not answer its project list (unreachable, a pohunek error, a
timeout) is skipped, and the next host in `hosts` is asked. Each skip is logged
at WARN as `review host skipped` with `host`, `repository`, `error_code` and
`skipped_after_failure`. A host that failed is not asked again in the same
discovery pass (`skipped_after_failure` is then `true`), so a dead host costs one
`pohunek.timeout_seconds` per pass, not one per pull request. A host whose outage
was announced is asked once per pass even when no review is due, until it answers
or leaves `hosts`, so its recovery ends the outage.

A failing host never stops the reviews of repositories that other hosts serve.
When no later host has the repository, the review waits for a review host, not
`project_route_not_found`. It uses no retry of
[`schedule.retry_delays_seconds`](../../configuration.md#schedule), however long
the outage lasts, and starts on the first discovery pass after a host answers,
without `autoreview retry`. One `review_stopped` message per host outage says
that reviews wait for a review host, names the failed hosts and the host error
code, for example
`Diagnostic: pohunek_request_timeout.`, and the review queue lists the review as
`AI review waits for a review host: CODE` (see
[waiting for a review host](../concepts/state-and-recovery.md#waiting-for-a-review-host)).

## Register each repository

Register the repository as a pohunek project on each host that may review it.
Run the registration on that host:

```sh
pohunek project add <repository-path>
```

A repository registered on one host always goes there. A repository
registered on several hosts goes to the first of them in `hosts`, and to the
next one when that host fails. Keep one registration per repository on each
host: two projects of the same repository on one host are
`project_route_ambiguous`.

A layout that splits repositories over hosts works well when the repositories
differ in size or in the tools they need:

| Host | Registered repositories |
|---|---|
| `review-host-a` | `zajca/pohunek`, `pohunekd/pohunek-companion` |
| `review-host-b` | `pohunekd/pohunek.dev`, `pohunekd/pohunek-companion-android` |

## Pin a repository with a route override

Use `route_overrides` when a repository must always run on one host and
project, for example a copy kept for testing on a lower-priority host, or two
projects of the repository on one host. The override names the host and the
pohunek project id that must be used:

```yaml
pohunek:
  hosts: [review-host-a, review-host-b]
  route_overrides:
    zajca/pohunek:
      host: review-host-a
      project_id: <project-id>
```

- The key is the lowercase `owner/repo`.
- `host` must be one of `hosts`. `project_id` is the id of the project on that
  host, as pohunek reports it.
- An override replaces the search for that repository only: only the named
  host is asked. It still requires the project to exist on the named host; a
  missing project is an error, not a silent fallback to another host, and a
  failing named host makes the review wait for its next retry.

Use an override to pin a route. Do not use it to hide a missing
registration: a repository that is registered nowhere still fails with
`project_route_not_found`, which is the signal you need.

## Install the agent profiles on every host

A reviewer runs an agent profile on the host where its review runs. A review
runs on the host where its repository is registered, so every host that can
receive a review of the policy's repositories needs every profile the policy
names. Run [`autoreview profiles install`](../../cli.md#autoreview-profiles-install)
on each of those hosts, as the user of its pohunek daemon: it writes the profile
and its adapter only for the daemon of the machine it runs on, and refuses a
`--host` that names another machine (`profile_host_not_local`).

The base runtime (`claude` or `codex`) must already be the
official pohunek runtime package on each host. See
[reviewers and adapters](../concepts/reviewers-and-adapters.md).

Run `autoreview validate` after a host is added. The output lists the agent
profiles the policy needs; compare it with what you installed.

## Reach a remote host over an overlay network

A review host does not have to sit next to the autoreview service. pohunek can
reach remote hosts over a private overlay network such as NetBird or Tailscale,
and autoreview then uses the host by its pohunek name like any other. Set up
the overlay and the pohunek remote host as described in the
[pohunek remote hosts guide](https://pohunek.dev/docs/guides/remote-hosts/).
autoreview needs nothing extra for this; it only sees the host name in `hosts`.

A remote host adds latency to every pohunek call. The `pohunek.timeout_seconds`
budget covers each call, so a slow link can cause `pohunek_request_timeout`;
raise the timeout before you blame the host.

## Capacity

`schedule.max_concurrent_reviews` limits how many reviews run at once in one
autoreview service, across all hosts. The example configuration sets 8. Each review
is a full agent run with its own worktree, so raise the value only when the
hosts can run that many sessions. If one host receives every review, check the
registrations: a repository registered on one host only always goes there. The
order of `hosts` is a priority, not load balancing: every review of a
repository goes to the first host that has it while that host answers.

For review limits and session timeouts, see
[configuration](../../configuration.md#schedule) and
[configuration session](../../configuration.md#session).

## Troubleshooting

| Error | Cause | Fix |
|---|---|---|
| `project_route_not_found` | No host that answered has the repository as a project allowed by `project_sources` | Run `pohunek project add` on the host that should review it, or allow the source in the pipeline |
| `project_route_ambiguous` | The host that serves the repository has it as two or more projects | Remove the extra registration, or add a `route_overrides` entry |
| `review host skipped` (WARN) | A host did not answer its project list; the next host was asked | Check the host and the overlay |
| `review host skipped` (WARN) with `host_state_ineligible` | A `pohunek.host_state` rule, named by `rule`, refused the host | Wait for the host's state to change, or relax the rule |
| `review host state unknown` (WARN) | The host's daemon could not run the plugin's command or stopped it at `read_timeout_seconds`, its state did not read as `HostCapacity` v1, or its timestamp was too far from now, either way | Check that the host runs the `moc.host-state` plugin and that its clock is right; a plugin that often hangs needs a fix on the host |
| `review host state not read` (WARN) | The state read failed as a pohunek call; the host counts as failed for the pass | Check the host and the overlay, as for a failed project list; a slow link can make the answer of a stopped plugin miss the CLI's extra 1 s |
| "Reviews wait for a review host" (`review_stopped`) | No host that may serve the repository answered; the review waits and starts once one does | Check the host, the overlay and the pohunek versions; no `autoreview retry` is needed |
| `pohunek_request_timeout` | A host did not answer in time | Check the host and the overlay; raise `pohunek.timeout_seconds` for slow links |
| `pohunek_session_start_failed` | The host could not start a session | Check the host's pohunek daemon and the profile installed on it |

The full list is in the [CLI error codes](../../cli.md#error-codes), and the
[troubleshooting runbook](../runbooks/troubleshooting.md) maps them to fixes.
