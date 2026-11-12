//! Discovery: the review hosts that the host-state rules allow, in the order they set.
//!
//! Each host's state is read at most once per discovery pass, from the `moc.host-state` plugin.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;
use std::future::Future;

use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::route::{HostFailure, HostFailures};
use crate::config::{HostOrder, HostStateChoice, HostStateRules};
use crate::engine::is_permanent;
use crate::error::{Error, error_code};
use crate::model::host_state::HOST_STATE_INVALID;
use crate::model::{HOST_STATE_UNAVAILABLE, HostCapacity};

/// Error code of a review that the state of no host allows.
pub(crate) const HOST_STATE_INELIGIBLE: &str = "host_state_ineligible";

/// Error code of a state whose timestamp is further from now than `max_age_seconds`.
const HOST_STATE_STALE: &str = "host_state_stale";

/// Error codes of a state read that reached the host but gave no usable document.
///
/// Any other failed read is a host failure of the pass, as a failed project list is.
const STATE_UNKNOWN_CODES: [&str; 2] = [HOST_STATE_INVALID, HOST_STATE_UNAVAILABLE];

/// What the host-state rules make of one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostVerdict {
    /// The host may run reviews; idle hosts come first in idle-first order.
    Eligible {
        /// Whether the host reported itself idle.
        idle: bool,
    },
    /// The host is not asked for reviews.
    Ineligible {
        /// Configuration key of the rule that refused the host.
        rule: &'static str,
    },
}

/// The verdicts of one discovery pass, keyed by host.
pub(crate) type HostVerdicts = BTreeMap<String, HostVerdict>;

/// The hosts of one route search after the host-state rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StateHosts {
    /// The hosts to try, in order.
    pub(crate) eligible: Vec<String>,
    /// The hosts that a rule refused, in configured order, each with the key of the rule.
    pub(crate) refused: Vec<(String, &'static str)>,
}

/// Returns the hosts of `hosts` that the rules allow, in the order the rules set.
///
/// `capacity` reads the state of one host. A host's verdict is kept in `verdicts`, so each host
/// is read at most once per pass. A host in `failures` is not read and stays a candidate that is
/// not idle, so the route search replays its failure. A read that fails as a pohunek call adds
/// the host to `failures` the same way. Each failed or unknown read logs one WARN line.
///
/// # Errors
///
/// Returns the error of `capacity` at once when `cancel` is cancelled.
pub(crate) async fn state_hosts<'a, F, Fut>(
    hosts: &'a [String],
    rules: &HostStateRules,
    now: OffsetDateTime,
    cancel: &CancellationToken,
    verdicts: &mut HostVerdicts,
    failures: &mut HostFailures,
    mut capacity: F,
) -> Result<StateHosts, Error>
where
    F: FnMut(&'a str) -> Fut,
    Fut: Future<Output = Result<HostCapacity, Error>>,
{
    let mut idle = Vec::new();
    let mut others = Vec::new();
    let mut refused = Vec::new();
    for host in hosts {
        let verdict = match verdicts.get(host) {
            Some(verdict) => *verdict,
            None if failures.contains_key(host) => HostVerdict::Eligible { idle: false },
            None => {
                let state = match capacity(host).await {
                    Ok(state) => Ok(state),
                    Err(err) if cancel.is_cancelled() => return Err(err),
                    Err(err) if !STATE_UNKNOWN_CODES.contains(&error_code(&err)) => {
                        // The call itself failed, as a project list can: the host is out.
                        let code = error_code(&err).to_owned();
                        tracing::warn!(
                            host = host.as_str(),
                            error_code = code.as_str(),
                            "review host state not read"
                        );
                        let permanent = is_permanent(&err);
                        failures.insert(host.clone(), HostFailure { code, permanent });
                        others.push(host.clone());
                        continue;
                    }
                    Err(err) => Err(error_code(&err).to_owned()),
                };
                let unknown = match &state {
                    Err(code) => Some(code.as_str()),
                    Ok(known) if stale(rules, known, now) => Some(HOST_STATE_STALE),
                    Ok(_) => None,
                };
                if let Some(code) = unknown {
                    tracing::warn!(
                        host = host.as_str(),
                        error_code = code,
                        "review host state unknown"
                    );
                }
                let verdict = judge(rules, &state, now);
                verdicts.insert(host.clone(), verdict);
                verdict
            }
        };
        match verdict {
            HostVerdict::Eligible { idle: true } if rules.order == HostOrder::IdleFirst => {
                idle.push(host.clone());
            }
            HostVerdict::Eligible { .. } => others.push(host.clone()),
            HostVerdict::Ineligible { rule } => refused.push((host.clone(), rule)),
        }
    }
    idle.append(&mut others);
    Ok(StateHosts {
        eligible: idle,
        refused,
    })
}

/// Returns the verdict of the rules on one host's `state` at `now`.
///
/// `state` is the host's capacity, or the error code of a read that failed. A failed read and a
/// stale state are unknown, which `unknown_state` decides; an eligible unknown host is not idle.
/// The first rule that refuses the host names the verdict.
pub(crate) fn judge(
    rules: &HostStateRules,
    state: &Result<HostCapacity, String>,
    now: OffsetDateTime,
) -> HostVerdict {
    let capacity = match state {
        Ok(capacity) if !stale(rules, capacity, now) => capacity,
        _ => {
            return match rules.unknown_state {
                HostStateChoice::Skip => HostVerdict::Ineligible {
                    rule: "unknown_state",
                },
                HostStateChoice::Eligible => HostVerdict::Eligible { idle: false },
            };
        }
    };
    let on_battery = !capacity.ac_power;
    let models = &capacity.local_models;
    let rule = if on_battery && rules.on_battery == HostStateChoice::Skip {
        Some("on_battery")
    } else if on_battery
        && capacity
            .battery_percent
            .is_some_and(|percent| percent < rules.min_battery_percent)
    {
        Some("min_battery_percent")
    } else if capacity.memory.free_mb < rules.min_free_memory_mb {
        Some("min_free_memory_mb")
    } else if !rules.required_models.is_empty()
        && (!models.reachable
            || rules
                .required_models
                .iter()
                .any(|model| !models.models.contains(model)))
    {
        Some("required_models")
    } else if !capacity.policy.inside_window && rules.outside_window == HostStateChoice::Skip {
        Some("outside_window")
    } else {
        None
    };
    match rule {
        Some(rule) => HostVerdict::Ineligible { rule },
        None => HostVerdict::Eligible {
            idle: capacity.idle,
        },
    }
}

/// Reports whether a state's timestamp is further than `max_age` from `now`, before or after.
///
/// A timestamp slightly ahead of `now` is a host clock that runs fast, not a stale state.
fn stale(rules: &HostStateRules, capacity: &HostCapacity, now: OffsetDateTime) -> bool {
    (now - capacity.timestamp).unsigned_abs() > rules.max_age
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::future::ready;
    use std::time::Duration;

    use time::OffsetDateTime;
    use time::macros::datetime;
    use tokio_util::sync::CancellationToken;

    use super::{HostVerdict, HostVerdicts, StateHosts, judge, state_hosts};
    use crate::config::{HostOrder, HostStateChoice, HostStateRules};
    use crate::discovery::route::{HostFailure, HostFailures};
    use crate::error::{Error, error_code};
    use crate::model::HostCapacity;

    /// A real `HostCapacity` document of a desktop host, with sanitized names.
    const SAMPLE: &str = r#"{
  "schemaVersion": "v1",
  "hostAlias": "host-a",
  "timestamp": "2026-11-11T23:58:30.157Z",
  "idle": true,
  "idleReason": "no user input for 137 minutes, on AC power",
  "secondsSinceInput": 8246,
  "acPower": true,
  "batteryPercent": null,
  "cpu": {"cores": 24, "load1m": 0.43},
  "memory": {"totalMb": 61921, "freeMb": 39665},
  "gpu": {"present": true, "name": "example-gpu", "freeMb": 269},
  "localModels": {"reachable": false, "models": []},
  "policy": {
    "allowedWindow": {"start": "00:00", "end": "00:00"},
    "insideWindow": true,
    "maxConcurrentAgents": 2
  }
}"#;

    /// A case of the rule table: its name, the state document, the rule changes and the verdict.
    type Case = (&'static str, String, fn(&mut HostStateRules), HostVerdict);

    /// The instant the cases judge at: one minute after the sample was read.
    const NOW: OffsetDateTime = datetime!(2026-11-11 23:59:30.157 UTC);

    /// Rules that every case starts from: every limit set, every choice `skip`.
    fn rules() -> HostStateRules {
        HostStateRules {
            max_age: Duration::from_secs(120),
            unknown_state: HostStateChoice::Skip,
            order: HostOrder::IdleFirst,
            on_battery: HostStateChoice::Skip,
            min_battery_percent: 30,
            min_free_memory_mb: 8192,
            required_models: Vec::new(),
            outside_window: HostStateChoice::Skip,
        }
    }

    /// Returns the state that `text` reads as, or the error code of its read.
    fn read(text: &str) -> Result<HostCapacity, String> {
        HostCapacity::parse(text).map_err(|err| error_code(&err).to_owned())
    }

    /// Returns the sample with its first `from` replaced by `to`; the text must change.
    fn edited(from: &str, to: &str) -> String {
        assert!(SAMPLE.contains(from), "the sample holds {from:?}");
        SAMPLE.replacen(from, to, 1)
    }

    /// A host that runs on battery at 25 percent.
    fn battery() -> String {
        edited("\"acPower\": true", "\"acPower\": false").replacen(
            "\"batteryPercent\": null",
            "\"batteryPercent\": 25",
            1,
        )
    }

    #[test]
    fn host_state_rules_judge_each_state() {
        let eligible = HostVerdict::Eligible { idle: true };
        let busy = HostVerdict::Eligible { idle: false };
        let refused = |rule| HostVerdict::Ineligible { rule };
        let models = "\"localModels\": {\"reachable\": true, \"models\": [\"model-a\"]}";
        let no_models = "\"localModels\": {\"reachable\": false, \"models\": []}";
        let skip = HostStateChoice::Skip;
        let allow = HostStateChoice::Eligible;
        // (case, state document, rule changes, expected verdict)
        let cases: Vec<Case> = vec![
            ("the sample", SAMPLE.to_owned(), |_| {}, eligible),
            (
                "busy",
                edited("\"idle\": true", "\"idle\": false"),
                |_| {},
                busy,
            ),
            (
                "on battery, skipped",
                battery(),
                |_| {},
                refused("on_battery"),
            ),
            (
                "on battery below the limit",
                battery(),
                |r| r.on_battery = HostStateChoice::Eligible,
                refused("min_battery_percent"),
            ),
            (
                "on battery at the limit",
                battery(),
                |r| {
                    r.on_battery = HostStateChoice::Eligible;
                    r.min_battery_percent = 25;
                },
                eligible,
            ),
            (
                "on battery, no limit",
                battery(),
                |r| {
                    r.on_battery = HostStateChoice::Eligible;
                    r.min_battery_percent = 0;
                },
                eligible,
            ),
            (
                "low battery on mains power",
                edited("\"batteryPercent\": null", "\"batteryPercent\": 5"),
                |_| {},
                eligible,
            ),
            (
                "free memory below the limit",
                SAMPLE.to_owned(),
                |r| r.min_free_memory_mb = 39_666,
                refused("min_free_memory_mb"),
            ),
            (
                "free memory at the limit",
                SAMPLE.to_owned(),
                |r| r.min_free_memory_mb = 39_665,
                eligible,
            ),
            (
                "required model served",
                edited(no_models, models),
                |r| r.required_models = vec!["model-a".to_owned()],
                eligible,
            ),
            (
                "required model missing",
                edited(no_models, models),
                |r| r.required_models = vec!["model-a".to_owned(), "model-b".to_owned()],
                refused("required_models"),
            ),
            (
                "model server unreachable",
                edited(no_models, &models.replacen("true", "false", 1)),
                |r| r.required_models = vec!["model-a".to_owned()],
                refused("required_models"),
            ),
            (
                "outside the window, skipped",
                edited("\"insideWindow\": true", "\"insideWindow\": false"),
                |_| {},
                refused("outside_window"),
            ),
            (
                "outside the window, allowed",
                edited("\"insideWindow\": true", "\"insideWindow\": false"),
                |r| r.outside_window = HostStateChoice::Eligible,
                eligible,
            ),
            (
                "state at the age limit",
                edited("23:58:30.157Z", "23:57:30.157Z"),
                |_| {},
                eligible,
            ),
            (
                "stale state",
                edited("23:58:30.157Z", "23:57:30.156Z"),
                |_| {},
                refused("unknown_state"),
            ),
            (
                "state from a host clock one second ahead",
                edited("23:58:30.157Z", "23:59:31.157Z"),
                |_| {},
                eligible,
            ),
            (
                "state from the future",
                edited("23:58:30.157Z", "00:02:00Z").replacen("11T", "12T", 1),
                |_| {},
                refused("unknown_state"),
            ),
            (
                "stale state, unknown allowed",
                edited("23:58:30.157Z", "23:00:00Z"),
                |r| r.unknown_state = HostStateChoice::Eligible,
                busy,
            ),
            (
                "timestamp with an offset",
                edited("23:58:30.157Z", "23:58:30.157+00:00"),
                |_| {},
                refused("unknown_state"),
            ),
            (
                "another schema version",
                edited("\"v1\"", "\"v2\""),
                |_| {},
                refused("unknown_state"),
            ),
            (
                "unknown field",
                edited("\"idle\": true", "\"idle\": true, \"extra\": 1"),
                |_| {},
                refused("unknown_state"),
            ),
            (
                "unknown nested field",
                edited("\"freeMb\": 39665", "\"freeMb\": 39665, \"usedMb\": 1"),
                |_| {},
                refused("unknown_state"),
            ),
            (
                "missing nullable field",
                edited("\"batteryPercent\": null,", ""),
                |r| r.unknown_state = HostStateChoice::Eligible,
                busy,
            ),
            (
                "battery above 100",
                edited("\"batteryPercent\": null", "\"batteryPercent\": 101"),
                |_| {},
                refused("unknown_state"),
            ),
            (
                "not JSON",
                "host-state: unavailable".to_owned(),
                |r| r.unknown_state = HostStateChoice::Eligible,
                busy,
            ),
        ];
        for (case, text, change, want) in cases {
            let mut rules = rules();
            change(&mut rules);
            assert_eq!(judge(&rules, &read(&text), NOW), want, "{case}");
        }
        assert_eq!(
            read("{}").map(|_| ()),
            Err("host_state_invalid".to_owned()),
            "a document that does not read is invalid"
        );
        let failed = Err("host_state_unavailable".to_owned());
        let mut rules = rules();
        for (choice, want) in [(skip, refused("unknown_state")), (allow, busy)] {
            rules.unknown_state = choice;
            assert_eq!(
                judge(&rules, &failed, NOW),
                want,
                "a failed read, {choice:?}"
            );
        }
    }

    #[tokio::test]
    async fn hosts_are_ordered_idle_first_and_each_state_is_read_once_per_pass() {
        let cancel = CancellationToken::new();
        let hosts: Vec<String> = ["a", "b", "c", "d", "e", "f"].map(str::to_owned).to_vec();
        let sample = read(SAMPLE).expect("the sample reads");
        let busy = HostCapacity {
            idle: false,
            ..sample.clone()
        };
        let on_battery = HostCapacity {
            ac_power: false,
            ..sample.clone()
        };
        // a busy, b on battery, c idle, d not reached, e idle, f without the plugin.
        let states: BTreeMap<&str, HostCapacity> = [
            ("a", busy),
            ("b", on_battery),
            ("c", sample.clone()),
            ("e", sample),
        ]
        .into_iter()
        .collect();
        // (order, expected eligible hosts, expected refused hosts)
        let cases = [
            (
                HostOrder::IdleFirst,
                ["c", "e", "a", "d"],
                [("b", "on_battery"), ("f", "unknown_state")],
            ),
            (
                HostOrder::Configured,
                ["a", "c", "d", "e"],
                [("b", "on_battery"), ("f", "unknown_state")],
            ),
        ];
        for (order, eligible, refused) in cases {
            let rules = HostStateRules { order, ..rules() };
            let want = StateHosts {
                eligible: eligible.map(str::to_owned).to_vec(),
                refused: refused.map(|(host, rule)| (host.to_owned(), rule)).to_vec(),
            };
            let mut verdicts = HostVerdicts::new();
            let mut failures = HostFailures::new();
            let mut reads = Vec::new();
            for pass_resolve in 0..2 {
                let result = state_hosts(
                    &hosts,
                    &rules,
                    NOW,
                    &cancel,
                    &mut verdicts,
                    &mut failures,
                    |host| {
                        reads.push(host.to_owned());
                        ready(match states.get(host) {
                            Some(state) => Ok(state.clone()),
                            None if host == "f" => {
                                Err(Error::workflow("host_state_unavailable", false, None))
                            }
                            None => Err(Error::workflow("pohunek_request_failed", false, None)),
                        })
                    },
                )
                .await;
                assert_eq!(result.ok(), Some(want.clone()), "{order:?} {pass_resolve}");
            }
            assert_eq!(reads, hosts, "{order:?}: each host is read once per pass");
            let failed: Vec<&String> = failures.keys().collect();
            assert_eq!(
                failed,
                ["d"],
                "{order:?}: a read that fails as a call is a host failure"
            );
            assert_eq!(failures["d"].code, "pohunek_request_failed", "{order:?}");
        }
        let mut failures = HostFailures::new();
        failures.insert(
            "a".to_owned(),
            HostFailure {
                code: "pohunek_request_timeout".to_owned(),
                permanent: false,
            },
        );
        let mut reads = Vec::new();
        let result = state_hosts(
            &hosts[..2],
            &rules(),
            NOW,
            &cancel,
            &mut HostVerdicts::new(),
            &mut failures,
            |host| {
                reads.push(host.to_owned());
                ready(Ok(states["b"].clone()))
            },
        )
        .await;
        let want = StateHosts {
            eligible: vec!["a".to_owned()],
            refused: vec![("b".to_owned(), "on_battery")],
        };
        assert_eq!(result.ok(), Some(want), "a failed host stays a candidate");
        assert_eq!(reads, ["b"], "a host that failed in the pass is not read");
        cancel.cancel();
        let mut verdicts = HostVerdicts::new();
        let result = state_hosts(
            &hosts,
            &rules(),
            NOW,
            &cancel,
            &mut verdicts,
            &mut HostFailures::new(),
            |_| ready(Err(Error::workflow("pohunek_request_failed", false, None))),
        )
        .await;
        assert_eq!(
            result.map_err(|err| error_code(&err).to_owned()),
            Err("pohunek_request_failed".to_owned()),
            "a cancelled read returns its error"
        );
        assert!(verdicts.is_empty(), "verdicts={verdicts:?}");
    }
}
