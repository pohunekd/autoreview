//! The rule that decides whether a pull request's pending review requests ask a credential.

// Rust guideline compliant 2026-06-26

/// One pending review request of a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedReviewer {
    /// A request to a user, by login.
    User(String),
    /// A request to a team, by its combined slug `org/team`; empty when it was not read.
    Team(String),
    /// A request to a reviewer the token cannot read.
    Unreadable,
}

/// What the pending review requests of a pull request mean for one credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    /// The pull request asks the credential's login, directly or through a listed team.
    Requested,
    /// The pull request asks only a team that is not listed, or a reviewer that cannot be read.
    TeamOnly,
    /// No pending request concerns the credential.
    NotRequested,
}

/// Decides what `reviewers` mean for the account `login`, which lists `teams` as its own.
///
/// A user equal to `login` or a team equal to an entry of `teams` is a request. Otherwise a team
/// or an unreadable reviewer is a team-only request. Comparisons ignore ASCII case, and a team
/// with an empty slug never matches. Whether a teammate has already answered a team-only request
/// is not decided here.
#[must_use]
pub fn request_status(
    reviewers: &[RequestedReviewer],
    login: &str,
    teams: &[String],
) -> RequestStatus {
    let requested = reviewers.iter().any(|reviewer| match reviewer {
        RequestedReviewer::User(user) => user.eq_ignore_ascii_case(login),
        RequestedReviewer::Team(slug) => team_listed(slug, teams),
        RequestedReviewer::Unreadable => false,
    });
    if requested {
        return RequestStatus::Requested;
    }
    let team_only = reviewers.iter().any(|reviewer| {
        matches!(
            reviewer,
            RequestedReviewer::Team(_) | RequestedReviewer::Unreadable
        )
    });
    if team_only {
        RequestStatus::TeamOnly
    } else {
        RequestStatus::NotRequested
    }
}

/// Reports whether the combined team slug `slug` is one of `teams`, ignoring ASCII case.
///
/// An empty slug, a team whose name was not read, is never listed.
#[must_use]
pub fn team_listed(slug: &str, teams: &[String]) -> bool {
    !slug.is_empty() && teams.iter().any(|team| team.eq_ignore_ascii_case(slug))
}

#[cfg(test)]
mod tests {
    use super::{RequestStatus, RequestedReviewer, request_status};

    fn user(login: &str) -> RequestedReviewer {
        RequestedReviewer::User(login.to_owned())
    }

    fn team(slug: &str) -> RequestedReviewer {
        RequestedReviewer::Team(slug.to_owned())
    }

    #[test]
    fn request_status_table() {
        let listed = vec!["acme/backend".to_owned()];
        let cases = [
            (
                "direct login in mixed case",
                vec![user("Review-Bot")],
                listed.clone(),
                RequestStatus::Requested,
            ),
            (
                "listed team in mixed case",
                vec![team("Acme/Backend")],
                listed.clone(),
                RequestStatus::Requested,
            ),
            (
                "unlisted team",
                vec![team("acme/frontend")],
                listed.clone(),
                RequestStatus::TeamOnly,
            ),
            (
                "team without a slug",
                vec![team("")],
                listed.clone(),
                RequestStatus::TeamOnly,
            ),
            (
                "unreadable reviewer",
                vec![RequestedReviewer::Unreadable],
                listed.clone(),
                RequestStatus::TeamOnly,
            ),
            (
                "no reviewers",
                vec![],
                listed.clone(),
                RequestStatus::NotRequested,
            ),
            (
                "another user only",
                vec![user("someone-else")],
                listed.clone(),
                RequestStatus::NotRequested,
            ),
            (
                "listed team beside an unreadable reviewer",
                vec![RequestedReviewer::Unreadable, team("acme/backend")],
                listed.clone(),
                RequestStatus::Requested,
            ),
            (
                "team request without listed teams",
                vec![team("acme/backend")],
                vec![],
                RequestStatus::TeamOnly,
            ),
        ];
        for (name, reviewers, teams, expected) in cases {
            assert_eq!(
                request_status(&reviewers, "review-bot", &teams),
                expected,
                "{name}"
            );
        }
    }
}
