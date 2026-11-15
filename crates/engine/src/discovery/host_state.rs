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
