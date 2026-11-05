//! Anchoring of findings to the changed lines of a pull request.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeMap;

use crate::model::{Artifact, Finding};

/// Marks every finding on a changed RIGHT-side line as anchored.
///
/// A finding elsewhere in a changed file moves to the closest changed line and says where it
/// was reported. A finding in a file the pull request does not change stays unanchored, is
/// listed in the review body and never blocks.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S28"))]
pub(crate) fn anchor_findings(
    mut artifact: Artifact,
    commentable: &BTreeMap<String, Vec<i64>>,
) -> Artifact {
    for finding in &mut artifact.findings {
        let lines = commentable
            .get(&finding.path)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if contains_line(lines, finding.line) {
            finding.anchored = true;
            continue;
        }
        let line = nearest_line(lines, finding.line);
        if line == 0 {
            finding.anchored = false;
            finding.non_blocking = true;
            continue;
        }
        anchor_to(finding, line);
    }
    artifact
}

/// Moves a finding to a changed line and notes the line it was reported at.
fn anchor_to(finding: &mut Finding, line: i64) {
    let original = finding.line;
    let previous = finding.body.clone();
    finding.body = format!(
        "Reported at `{}:{original}`, which is not a changed line of this pull \
         request; anchored to the closest changed line.\n\n{previous}",
        finding.path
    );
    finding.line = line;
    finding.anchored = true;
}

/// Reports whether `target` is one of the changed lines.
fn contains_line(lines: &[i64], target: i64) -> bool {
    lines.contains(&target)
}

/// Returns the closest line to `target`, or 0 when there is none.
///
/// On a tie the lower line wins. Only positive lines are candidates. Distances wrap on overflow,
/// as Go's `int` does.
fn nearest_line(lines: &[i64], target: i64) -> i64 {
    let mut best = 0;
    for &line in lines {
        if line <= 0 {
            continue;
        }
        if best == 0 {
            best = line;
            continue;
        }
        let dist_line = if line > target {
            line.wrapping_sub(target)
        } else {
            target.wrapping_sub(line)
        };
        let dist_best = if best > target {
            best.wrapping_sub(target)
        } else {
            target.wrapping_sub(best)
        };
        if dist_line < dist_best || (dist_line == dist_best && line < best) {
            best = line;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::anchor_findings;
    use crate::model::{Artifact, Finding};

    #[test]
    fn anchor_findings_moves_within_the_file_and_keeps_other_files_in_the_body() {
        let artifact = Artifact {
            findings: vec![
                Finding {
                    id: "O001".to_owned(),
                    path: "src/a.go".to_owned(),
                    line: 10,
                    body: "exact".to_owned(),
                    ..Finding::default()
                },
                Finding {
                    id: "O002".to_owned(),
                    path: "src/a.go".to_owned(),
                    line: 14,
                    body: "near".to_owned(),
                    ..Finding::default()
                },
                Finding {
                    id: "O003".to_owned(),
                    path: "src/unchanged.go".to_owned(),
                    line: 3,
                    body: "elsewhere".to_owned(),
                    ..Finding::default()
                },
            ],
            ..Artifact::default()
        };
        let commentable = BTreeMap::from([("src/a.go".to_owned(), vec![10, 12, 20])]);
        let anchored = anchor_findings(artifact, &commentable);
        assert!(
            anchored.findings[0].anchored
                && anchored.findings[0].line == 10
                && anchored.findings[0].body == "exact",
            "exact finding changed: {:?}",
            anchored.findings[0]
        );
        assert!(
            anchored.findings[1].anchored
                && anchored.findings[1].line == 12
                && anchored.findings[1]
                    .body
                    .contains("Reported at `src/a.go:14`"),
            "near finding not moved to the closest changed line: {:?}",
            anchored.findings[1]
        );
        assert!(
            !anchored.findings[2].anchored && anchored.findings[2].line == 3,
            "finding outside the diff must stay in the body: {:?}",
            anchored.findings[2]
        );
    }
}
