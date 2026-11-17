//! The eval report: trial results, recall and false blocking rates, the gate and the exit code.
//!
//! Only completed trials enter a rate; a failed trial is reported and decides the exit code.

// Rust guideline compliant 2026-06-26

use serde::Serialize;

use super::case::Case;
use super::matching::{ObservedFinding, TrialScore};
use crate::error::Error;

/// Schema name of the report.
pub const REPORT_SCHEMA: &str = "autoreview-eval-report/v1";

/// Error code of a threshold whose class of cases has no selected case.
pub const THRESHOLD_NO_DATA: &str = "eval_threshold_no_data";

/// How a trial ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialStatus {
    /// The review finished and its findings were scored.
    Completed,
    /// The review could not be measured; the trial enters no rate.
    Failed,
}

/// What the verifier did in a trial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierState {
    /// The pipeline has no verifier.
    None,
    /// The pipeline has a verifier and no blocking claim needed it.
    NotNeeded,
    /// The verifier checked the blocking claims.
    Ran,
    /// The verifier did not finish, so the claims stand unchecked.
    Failed,
}

/// One case reviewed once.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrialReport {
    /// Index of the repeat, from 1.
    pub repeat: i64,
    /// How the trial ended.
    pub status: TrialStatus,
    /// Error code of a failed trial; empty otherwise.
    pub error_code: String,
    /// Wall-clock time of the trial.
    pub duration_ms: i64,
    /// Sessions the trial started.
    pub sessions: u32,
    /// What the verifier did.
    pub verifier: VerifierState,
    /// The findings of the review.
    pub findings: Vec<ObservedFinding>,
    /// The score; absent for a failed trial.
    pub stages: Option<TrialScore>,
}

/// Whether a case expects blocking findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseKind {
    /// The case lists expected blocking findings.
    Defect,
    /// The case expects none.
    Clean,
}

/// The trials of one case in numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaseSummary {
    /// Trials run.
    pub trials: usize,
    /// Trials that were scored.
    pub completed: usize,
    /// Trials that failed.
    pub failed: usize,
    /// Completed trials that caught every expected finding, or blocked nothing on a clean case.
    pub passed_after: usize,
    /// Whether some completed trials passed and others did not.
    pub flaky: bool,
}

/// One case and its trials.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseReport {
    /// Identifier of the case.
    pub id: String,
    /// Whether the case expects blocking findings.
    pub expect: CaseKind,
    /// Whether the pipeline filter left the case out.
    pub skipped: bool,
    /// The trials, by repeat.
    pub trials: Vec<TrialReport>,
    /// The trials in numbers.
    pub summary: CaseSummary,
}

impl CaseReport {
    /// Builds the report of a case and counts its trials.
    #[must_use]
    pub fn new(id: String, expect: CaseKind, skipped: bool, trials: Vec<TrialReport>) -> Self {
        let scored = trials.iter().filter(|t| t.status == TrialStatus::Completed);
        let completed = scored.clone().count();
        let passed_after = scored
            .filter(|trial| {
                trial.stages.as_ref().is_some_and(|stages| {
                    let after = &stages.after_verification;
                    after.missed.is_empty() && after.false_blocking != Some(true)
                })
            })
            .count();
        let summary = CaseSummary {
            trials: trials.len(),
            completed,
            failed: trials.len() - completed,
            passed_after,
            flaky: passed_after > 0 && passed_after < completed,
        };
        Self {
            id,
            expect,
            skipped,
            trials,
            summary,
        }
    }
}

/// A count over a count and the quotient; the quotient is absent when the divisor is 0.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Ratio {
    /// The dividend.
    pub num: u64,
    /// The divisor.
    pub den: u64,
    /// The quotient, or `None` for a divisor of 0.
    pub value: Option<f64>,
}

impl Ratio {
    /// Returns `num` over `den`.
    #[must_use]
    pub fn new(num: u64, den: u64) -> Self {
        // The counts stay far below 2^52, so the conversion is exact.
        #[expect(clippy::cast_precision_loss, reason = "counts of trials are small")]
        let value = (den > 0).then(|| num as f64 / den as f64);
        Self { num, den, value }
    }
}

/// Recall over the completed trials of the defect cases.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DefectTotals {
    /// Completed trials of defect cases.
    pub completed_trials: u64,
    /// Expected findings over those trials.
    pub expected: u64,
    /// Expected findings caught before the verifier.
    pub caught_before: u64,
    /// Expected findings caught in the review the engine would publish.
    pub caught_after: u64,
    /// `caught_before` over `expected`.
    pub recall_before: Ratio,
    /// `caught_after` over `expected`.
    pub recall_after: Ratio,
}

/// False blocking over the completed trials of the clean cases.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CleanTotals {
    /// Completed trials of clean cases.
    pub completed_trials: u64,
    /// Trials with a blocking finding before the verifier.
    pub false_blocking_before: u64,
    /// Trials with a blocking finding in the review the engine would publish.
    pub false_blocking_after: u64,
    /// `false_blocking_before` over the completed trials.
    pub rate_before: Ratio,
    /// `false_blocking_after` over the completed trials.
    pub rate_after: Ratio,
}

/// The rates of a run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Totals {
    /// Recall of the defect cases.
    pub defect: DefectTotals,
    /// False blocking of the clean cases.
    pub clean: CleanTotals,
    /// Trials that could not be measured.
    pub failed_trials: u64,
}

/// Computes the rates of the cases that ran.
#[must_use]
pub fn totals(cases: &[CaseReport]) -> Totals {
    let (mut defect, mut clean, mut failed) = ([0_u64; 4], [0_u64; 3], 0_u64);
    for case in cases.iter().filter(|case| !case.skipped) {
        for trial in &case.trials {
            let Some(stages) = trial
                .stages
                .as_ref()
                .filter(|_| trial.status == TrialStatus::Completed)
            else {
                failed += 1;
                continue;
            };
            let (before, after) = (&stages.before_verification, &stages.after_verification);
            if case.expect == CaseKind::Defect {
                defect[0] += 1;
                defect[1] += (after.caught.len() + after.missed.len()) as u64;
                defect[2] += before.caught.len() as u64;
                defect[3] += after.caught.len() as u64;
            } else {
                clean[0] += 1;
                clean[1] += u64::from(before.false_blocking == Some(true));
                clean[2] += u64::from(after.false_blocking == Some(true));
            }
        }
    }
    Totals {
        defect: DefectTotals {
            completed_trials: defect[0],
            expected: defect[1],
            caught_before: defect[2],
            caught_after: defect[3],
            recall_before: Ratio::new(defect[2], defect[1]),
            recall_after: Ratio::new(defect[3], defect[1]),
        },
        clean: CleanTotals {
            completed_trials: clean[0],
            false_blocking_before: clean[1],
            false_blocking_after: clean[2],
            rate_before: Ratio::new(clean[1], clean[0]),
            rate_after: Ratio::new(clean[2], clean[0]),
        },
        failed_trials: failed,
    }
}

/// The thresholds of a run and whether the review configuration meets them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gate {
    /// Least recall after verification; absent when not asked for.
    pub min_recall: Option<f64>,
    /// Greatest false blocking rate after verification; absent when not asked for.
    pub max_false_blocking: Option<f64>,
    /// Whether every threshold given is met by a measured rate.
    pub passed: bool,
    /// One line per threshold that is not met or could not be measured.
    pub failures: Vec<String>,
    /// Whether a measured rate misses its threshold; `false` when every failure is a missing
    /// measurement. It only decides the exit code and is not part of the report.
    #[serde(skip)]
    pub regression: bool,
}

impl Gate {
    /// The gate of a run that was cancelled: it measured nothing and does not pass.
    #[must_use]
    pub fn not_evaluated(min_recall: Option<f64>, max_false_blocking: Option<f64>) -> Self {
        Self {
            min_recall,
            max_false_blocking,
            passed: false,
            failures: vec!["the run was cancelled before the thresholds were checked".to_owned()],
            regression: false,
        }
    }
}

/// Checks the rates after verification against the thresholds that are given.
///
/// A threshold whose class has no completed trial cannot be met, because every trial of the
/// class failed; it fails the gate without counting as a regression.
#[must_use]
pub fn evaluate_gate(
    totals: &Totals,
    min_recall: Option<f64>,
    max_false_blocking: Option<f64>,
) -> Gate {
    let mut failures = Vec::new();
    let mut regression = false;
    if let Some(min) = min_recall {
        match totals.defect.recall_after.value {
            Some(value) if value < min => {
                regression = true;
                failures.push(format!("recall {value:.3} is below {min}"));
            }
            Some(_) => {}
            None => failures
                .push("no defect case has a completed trial, so recall is unknown".to_owned()),
        }
    }
    if let Some(max) = max_false_blocking {
        match totals.clean.rate_after.value {
            Some(value) if value > max => {
                regression = true;
                failures.push(format!("false blocking rate {value:.3} is above {max}"));
            }
            Some(_) => {}
            None => failures.push(
                "no clean case has a completed trial, so the false blocking rate is unknown"
                    .to_owned(),
            ),
        }
    }
    Gate {
        min_recall,
        max_false_blocking,
        passed: failures.is_empty(),
        failures,
        regression,
    }
}

/// Refuses a run whose threshold has no case of its class under `pipeline`.
///
/// A run that starts with such a threshold could only fail, so it stops before any session.
///
/// # Errors
///
/// Returns `eval_threshold_no_data` when `min_recall` is given and no selected case is a
/// defect case, or `max_false_blocking` is given and no selected case is a clean case.
pub fn check_thresholds_have_cases(
    cases: &[Case],
    pipeline: &str,
    min_recall: Option<f64>,
    max_false_blocking: Option<f64>,
) -> Result<(), Error> {
    let selected = || cases.iter().filter(|case| case.runs_under(pipeline));
    let defect = selected().any(Case::is_defect);
    let clean = selected().any(|case| !case.is_defect());
    if (min_recall.is_some() && !defect) || (max_false_blocking.is_some() && !clean) {
        return Err(Error::workflow(THRESHOLD_NO_DATA, true, None));
    }
    Ok(())
}

/// Returns the exit code of a run that produced a report.
///
/// 2 when a measured rate misses its threshold, so a regression is never hidden by an
/// incident; 3 when a trial failed, a threshold could not be measured or the run was cancelled
/// (its gate is [`Gate::not_evaluated`]); 0 otherwise.
#[must_use]
pub fn exit_status(gate: &Gate, failed_trials: u64) -> u8 {
    if gate.regression {
        2
    } else if !gate.passed || failed_trials > 0 {
        3
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CaseKind, CaseReport, Gate, THRESHOLD_NO_DATA, TrialReport, TrialStatus, VerifierState,
        check_thresholds_have_cases, evaluate_gate, exit_status, totals,
    };
    use crate::eval::case::{Case, Expect, Expected};
    use crate::eval::matching::{StageScore, TrialScore};

    fn stage(caught: usize, missed: usize, blocks: Option<bool>) -> StageScore {
        StageScore {
            caught: (0..caught).collect(),
            missed: (0..missed)
                .map(|expected| crate::eval::matching::Missed {
                    expected,
                    reason: crate::eval::matching::MissReason::NotReported,
                })
                .collect(),
            false_blocking: blocks,
            ..StageScore::default()
        }
    }

    fn trial(status: TrialStatus, before: StageScore, after: StageScore) -> TrialReport {
        TrialReport {
            repeat: 1,
            status,
            error_code: String::new(),
            duration_ms: 0,
            sessions: 1,
            verifier: VerifierState::Ran,
            findings: Vec::new(),
            stages: (status == TrialStatus::Completed).then_some(TrialScore {
                before_verification: before,
                after_verification: after,
                moved_into_range: Vec::new(),
            }),
        }
    }

    /// Recall is micro-averaged over trials, failed trials enter no rate, a threshold without
    /// completed trials fails the gate with exit 3, a threshold without cases refuses the run,
    /// thresholds are inclusive, and a regression wins over an incident.
    #[test]
    fn rates_gate_and_exit_code_follow_the_completed_trials() {
        let done = TrialStatus::Completed;
        let defect = CaseReport::new(
            "d".to_owned(),
            CaseKind::Defect,
            false,
            vec![
                trial(done, stage(3, 0, None), stage(3, 0, None)),
                trial(done, stage(3, 0, None), stage(1, 2, None)),
                trial(TrialStatus::Failed, stage(0, 0, None), stage(0, 0, None)),
            ],
        );
        let clean = CaseReport::new(
            "c".to_owned(),
            CaseKind::Clean,
            false,
            vec![
                trial(done, stage(0, 0, Some(true)), stage(0, 0, Some(false))),
                trial(done, stage(0, 0, Some(false)), stage(0, 0, Some(false))),
            ],
        );
        assert!(defect.summary.flaky && !clean.summary.flaky);
        assert_eq!((defect.summary.completed, defect.summary.failed), (2, 1));
        let skipped = CaseReport::new("s".to_owned(), CaseKind::Clean, true, Vec::new());
        let all = [defect, clean, skipped];
        let totals = totals(&all);
        assert_eq!((totals.defect.expected, totals.defect.caught_after), (6, 4));
        assert_eq!(totals.defect.recall_before.value, Some(1.0));
        assert_eq!(totals.clean.rate_before.value, Some(0.5));
        assert_eq!(
            (totals.clean.false_blocking_after, totals.failed_trials),
            (0, 1)
        );
        let gate = |min, max| evaluate_gate(&totals, min, max);
        assert!(gate(None, None).passed);
        // Both boundaries are inclusive: recall 4/6 and false blocking 0/2.
        assert!(gate(Some(4.0 / 6.0), Some(0.0)).passed);
        assert!(!gate(Some(0.7), None).passed);
        assert_eq!(gate(Some(0.7), Some(0.0)).failures.len(), 1);
        assert_eq!(exit_status(&gate(None, None), totals.failed_trials), 3);
        assert_eq!(exit_status(&gate(Some(0.7), None), totals.failed_trials), 2);
        assert_eq!(exit_status(&gate(Some(0.5), None), 0), 0);
        // A class whose trials all failed has a null rate: its threshold fails the gate with
        // exit 3, unless the other class is a regression (exit 2).
        let broken = [
            CaseReport::new(
                "d".to_owned(),
                CaseKind::Defect,
                false,
                vec![trial(
                    TrialStatus::Failed,
                    stage(0, 0, None),
                    stage(0, 0, None),
                )],
            ),
            CaseReport::new(
                "c".to_owned(),
                CaseKind::Clean,
                false,
                vec![trial(
                    done,
                    stage(0, 0, Some(false)),
                    stage(0, 0, Some(false)),
                )],
            ),
        ];
        let none = super::totals(&broken);
        assert_eq!(none.defect.recall_after.value, None);
        assert_eq!(none.failed_trials, 1);
        let unknown = evaluate_gate(&none, Some(0.5), Some(0.0));
        assert!(!unknown.passed && !unknown.regression);
        assert_eq!(unknown.failures.len(), 1);
        assert_eq!(exit_status(&unknown, none.failed_trials), 3);
        let worse = evaluate_gate(&none, Some(0.5), Some(-0.1));
        assert_eq!((worse.failures.len(), exit_status(&worse, 1)), (2, 2));
        assert_eq!(evaluate_gate(&none, None, None).failures.len(), 0);
        // A cancelled run has no gate evaluation and exits 3 even without a failed trial.
        let cancelled = Gate::not_evaluated(Some(0.5), None);
        assert!(!cancelled.passed && !cancelled.regression);
        assert_eq!(exit_status(&cancelled, 0), 3);
        // Before any session, a threshold needs a selected case of its class.
        let case = |id: &str, expect, pipelines: &[&str]| Case {
            id: id.to_owned(),
            repository: "acme/widgets".to_owned(),
            pull_request: 1,
            base_branch: "main".to_owned(),
            head_ref: "refs/pull/1/head".to_owned(),
            base_sha: "a".repeat(40),
            head_sha: "b".repeat(40),
            pipelines: pipelines.iter().map(|name| (*name).to_owned()).collect(),
            title: String::new(),
            expect,
        };
        let wanted = Expect::Blocking(vec![Expected {
            path: "src/a.rs".to_owned(),
            first_line: 1,
            last_line: 2,
            description: "d".to_owned(),
        }]);
        let cases = [
            case("d", wanted, &["default"]),
            case("c", Expect::Clean, &["other"]),
        ];
        let check = |pipeline, min, max| check_thresholds_have_cases(&cases, pipeline, min, max);
        assert!(check("default", Some(0.5), None).is_ok());
        assert!(check("default", None, None).is_ok());
        assert!(check("other", None, Some(0.0)).is_ok());
        for (pipeline, min, max) in [
            ("default", None, Some(0.0)),
            ("other", Some(0.5), None),
            ("absent", Some(0.5), None),
        ] {
            let error = check(pipeline, min, max).expect_err("no case of the class");
            assert!(error.to_string().contains(THRESHOLD_NO_DATA), "{error}");
        }
    }
}
