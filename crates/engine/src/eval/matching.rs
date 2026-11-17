//! Matching of the findings of one review to the findings its case expects.
//!
//! An expected finding is caught when a finding that blocks at the stage has the same path and
//! a reported line inside the expected range. The reported line is the reviewer's own claim;
//! the line anchoring moved it to only decides `moved_into_range`.

// Rust guideline compliant 2026-06-26

use serde::Serialize;

use super::case::{Expect, Expected};

/// Why a finding that lies in an expected range does not block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissReason {
    /// No finding lies in the expected range.
    NotReported,
    /// The reviewer reported the finding as a note.
    ReportedAsNote,
    /// A `skip` glob of the repository review rules turned the finding into a note.
    SkippedByRules,
    /// The file has no changed line, so anchoring turned the finding into a note.
    NoteAfterAnchoring,
    /// The verifier refuted the finding or found it not blocking.
    DemotedByVerifier,
}

/// One finding of a trial, as the stored review steps tell it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservedFinding {
    /// Identifier of the finding within its review.
    pub id: String,
    /// Repository path of the file.
    pub path: String,
    /// Line the reviewer reported.
    pub reported_line: i64,
    /// Line the finding has after anchoring.
    pub line: i64,
    /// Severity the reviewer gave.
    pub severity: String,
    /// Title of the finding.
    pub title: String,
    /// Verdict of the verifier; empty when the finding was not verified.
    pub verdict: String,
    /// Whether the finding blocks when the verifier is given the claims.
    #[serde(rename = "blocking_before_verification")]
    pub blocking_before: bool,
    /// Whether the finding blocks in the review the engine would publish.
    #[serde(rename = "blocking_after_verification")]
    pub blocking_after: bool,
    /// The first step that turned the finding into a note; absent for a blocking finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<MissReason>,
}

/// The point of a review at which findings are counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The blocking findings that the verifier is given.
    BeforeVerification,
    /// The blocking findings of the review the engine would publish.
    AfterVerification,
}

/// An expected finding that no blocking finding caught.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Missed {
    /// Index of the expected finding in its case.
    pub expected: usize,
    /// Why it was missed.
    pub reason: MissReason,
}

/// What one trial found at one stage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StageScore {
    /// Findings that block at the stage.
    pub blocking: usize,
    /// Indexes of the expected findings that a blocking finding caught.
    pub caught: Vec<usize>,
    /// The expected findings that were not caught.
    pub missed: Vec<Missed>,
    /// Blocking findings outside every expected range; counted for information only.
    pub extra_blocking: usize,
    /// For a clean case, whether anything blocks; `None` for a defect case.
    pub false_blocking: Option<bool>,
}

/// What one trial found at both stages.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TrialScore {
    /// The stage before the verifier.
    pub before_verification: StageScore,
    /// The stage of the review the engine would publish.
    pub after_verification: StageScore,
    /// Blocking findings that only anchoring moved into an expected range.
    pub moved_into_range: Vec<String>,
}

/// Reports whether the reported line of `finding` lies in the range of `expected`.
fn in_range(finding: &ObservedFinding, expected: &Expected) -> bool {
    finding.path == expected.path
        && (expected.first_line..=expected.last_line).contains(&finding.reported_line)
}

/// Reports whether `finding` blocks at `stage`.
fn blocks(finding: &ObservedFinding, stage: Stage) -> bool {
    match stage {
        Stage::BeforeVerification => finding.blocking_before,
        Stage::AfterVerification => finding.blocking_after,
    }
}

/// Scores the findings of one trial at one stage against `expect`.
#[must_use]
pub fn score_stage(expect: &Expect, findings: &[ObservedFinding], stage: Stage) -> StageScore {
    let blocking: Vec<&ObservedFinding> = findings.iter().filter(|f| blocks(f, stage)).collect();
    let Expect::Blocking(wanted) = expect else {
        return StageScore {
            blocking: blocking.len(),
            false_blocking: Some(!blocking.is_empty()),
            ..StageScore::default()
        };
    };
    let (mut caught, mut missed) = (Vec::new(), Vec::new());
    for (index, expected) in wanted.iter().enumerate() {
        if blocking.iter().any(|f| in_range(f, expected)) {
            caught.push(index);
            continue;
        }
        let reason = (findings.iter().find(|f| in_range(f, expected))).and_then(|f| f.reason);
        let reason = reason.unwrap_or(MissReason::NotReported);
        missed.push(Missed {
            expected: index,
            reason,
        });
    }
    let extra_blocking = blocking
        .iter()
        .filter(|f| !wanted.iter().any(|e| in_range(f, e)))
        .count();
    StageScore {
        blocking: blocking.len(),
        caught,
        missed,
        extra_blocking,
        false_blocking: None,
    }
}

/// Scores the findings of one trial at both stages against `expect`.
#[must_use]
pub fn score_trial(expect: &Expect, findings: &[ObservedFinding]) -> TrialScore {
    let moved_into_range = match expect {
        Expect::Clean => Vec::new(),
        Expect::Blocking(wanted) => findings
            .iter()
            .filter(|f| f.blocking_after)
            .filter(|f| {
                let by_anchor = |e: &Expected| {
                    f.path == e.path && (e.first_line..=e.last_line).contains(&f.line)
                };
                wanted.iter().any(by_anchor) && !wanted.iter().any(|e| in_range(f, e))
            })
            .map(|f| f.id.clone())
            .collect(),
    };
    TrialScore {
        before_verification: score_stage(expect, findings, Stage::BeforeVerification),
        after_verification: score_stage(expect, findings, Stage::AfterVerification),
        moved_into_range,
    }
}

#[cfg(test)]
mod tests {
    use super::{MissReason, ObservedFinding, Stage, score_stage, score_trial};
    use crate::eval::case::{Expect, Expected};

    fn expected(path: &str, first_line: i64, last_line: i64) -> Expected {
        Expected {
            path: path.to_owned(),
            first_line,
            last_line,
            description: "d".to_owned(),
        }
    }

    fn finding(path: &str, reported: i64, line: i64, flags: (bool, bool)) -> ObservedFinding {
        let reason = match flags {
            (false, false) => Some(MissReason::ReportedAsNote),
            (true, false) => Some(MissReason::DemotedByVerifier),
            _ => None,
        };
        ObservedFinding {
            id: format!("O{reported}"),
            path: path.to_owned(),
            reported_line: reported,
            line,
            severity: "high".to_owned(),
            title: "t".to_owned(),
            verdict: String::new(),
            blocking_before: flags.0,
            blocking_after: flags.1,
            reason,
        }
    }

    /// An expected finding is caught by the path and the reported line, at each stage.
    #[test]
    fn expected_findings_are_matched_by_path_and_reported_line() {
        let wanted = Expect::Blocking(vec![
            expected("src/a.rs", 40, 52),
            expected("src/b.rs", 5, 5),
        ]);
        let both = (true, true);
        // (findings, caught after, missed after as (index, reason), extra blocking after)
        let rows = [
            (
                vec![finding("src/a.rs", 40, 40, both)],
                vec![0],
                vec![(1, MissReason::NotReported)],
                0,
            ),
            (
                vec![finding("src/a.rs", 52, 52, both)],
                vec![0],
                vec![(1, MissReason::NotReported)],
                0,
            ),
            (
                vec![finding("src/a.rs", 39, 40, both)],
                vec![],
                vec![(0, MissReason::NotReported), (1, MissReason::NotReported)],
                1,
            ),
            (
                vec![finding("src/a.rs", 53, 52, both)],
                vec![],
                vec![(0, MissReason::NotReported), (1, MissReason::NotReported)],
                1,
            ),
            (
                vec![finding("src/a.rs.bak", 45, 45, both)],
                vec![],
                vec![(0, MissReason::NotReported), (1, MissReason::NotReported)],
                1,
            ),
            (
                vec![finding("src/a.rs", 45, 45, (false, false))],
                vec![],
                vec![
                    (0, MissReason::ReportedAsNote),
                    (1, MissReason::NotReported),
                ],
                0,
            ),
            (
                vec![finding("src/a.rs", 45, 45, (true, false))],
                vec![],
                vec![
                    (0, MissReason::DemotedByVerifier),
                    (1, MissReason::NotReported),
                ],
                0,
            ),
            (
                vec![
                    finding("src/a.rs", 41, 41, both),
                    finding("src/a.rs", 42, 42, both),
                ],
                vec![0],
                vec![(1, MissReason::NotReported)],
                0,
            ),
            (
                vec![
                    finding("src/a.rs", 45, 45, both),
                    finding("src/b.rs", 5, 5, both),
                ],
                vec![0, 1],
                vec![],
                0,
            ),
        ];
        for (findings, caught, missed, extra) in rows {
            let after = score_stage(&wanted, &findings, Stage::AfterVerification);
            let got: Vec<(usize, MissReason)> = after
                .missed
                .iter()
                .map(|m| (m.expected, m.reason))
                .collect();
            assert_eq!(
                (after.caught, got, after.extra_blocking),
                (caught, missed, extra),
                "{findings:?}"
            );
        }
        // A demoted finding blocks before the verifier and not after it.
        let demoted = vec![finding("src/a.rs", 45, 45, (true, false))];
        let score = score_trial(&wanted, &demoted);
        assert_eq!(score.before_verification.caught, [0]);
        assert!(score.after_verification.caught.is_empty());
        // A finding that anchoring moved into the range earns nothing, and is reported.
        let moved = vec![finding("src/a.rs", 80, 45, both)];
        let score = score_trial(&wanted, &moved);
        assert!(score.after_verification.caught.is_empty());
        assert_eq!(score.moved_into_range, ["O80"]);
        // A clean case is falsely blocked by a blocking finding, never by a note.
        let clean = |findings: &[ObservedFinding]| {
            score_stage(&Expect::Clean, findings, Stage::AfterVerification)
        };
        assert_eq!(
            clean(&[finding("src/a.rs", 1, 1, (false, false))]).false_blocking,
            Some(false)
        );
        assert_eq!(
            clean(&[finding("src/a.rs", 1, 1, both)]).false_blocking,
            Some(true)
        );
    }
}
