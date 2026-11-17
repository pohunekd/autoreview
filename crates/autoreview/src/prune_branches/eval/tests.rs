use super::{EvalHostReport, SessionReport, incomplete};
use crate::prune_branches::RepositoryReport;
use crate::prune_branches::plan::{BranchPlan, Verdict};

/// A host that was checked: one removed session and one deleted branch.
fn host() -> EvalHostReport {
    let session = SessionReport {
        id: "s-1".to_owned(),
        name: "eval-prr-v2-opus-run1".to_owned(),
        branch: "autoreview/eval/prr-v2-opus-run1".to_owned(),
        state: "running".to_owned(),
        removed: true,
        error: String::new(),
    };
    let branch = BranchPlan {
        branch: "autoreview/eval/prr-v2-opus-run1".to_owned(),
        commit: format!("{:040}", 0),
        verdict: Verdict::Delete,
        deleted: true,
        error: String::new(),
    };
    let repository = RepositoryReport {
        path: "/srv/repos/example".to_owned(),
        error: String::new(),
        branches: vec![branch],
    };
    EvalHostReport {
        host: "review-a".to_owned(),
        skipped: String::new(),
        branches_skipped: String::new(),
        sessions: vec![session],
        repositories: vec![repository],
    }
}

#[test]
fn a_failed_removal_or_deletion_or_an_unchecked_named_host_fails_the_run() {
    let with = |change: fn(&mut EvalHostReport)| {
        let mut host = host();
        change(&mut host);
        host
    };
    let list_failed = || with(|host| host.skipped = "pohunek_command_failed".to_owned());
    let not_local = || with(|host| host.branches_skipped = "host_not_local".to_owned());
    let unread = || with(|host| host.repositories[0].error = "git_command_failed".to_owned());
    let removal = || with(|host| host.sessions[0].error = "session_removal_timed_out".to_owned());
    let deletion =
        || with(|host| host.repositories[0].branches[0].error = "git_command_failed".to_owned());
    let cases = [
        (vec![], false, false),
        (vec![host()], false, false),
        (vec![host()], true, false),
        (vec![host(), list_failed()], false, false),
        (vec![list_failed()], true, true),
        (vec![host(), not_local()], false, false),
        (vec![not_local()], true, true),
        (vec![host(), unread()], false, false),
        (vec![unread()], true, true),
        (vec![host(), removal()], false, true),
        (vec![removal()], true, true),
        (vec![host(), deletion()], false, true),
        (vec![deletion()], true, true),
    ];
    for (index, (hosts, named, want)) in cases.into_iter().enumerate() {
        assert_eq!(incomplete(&hosts, named), want, "case {index}");
    }
}
