//! The engine's side effects over the `gh` CLI, the pohunek CLI and Matrix.
//!
//! Mirrors `cmd/autoreview/integrations.go:109-130`. Each method of [`Integrations`] hands its
//! arguments to one client method, as the Go methods of `External` do.

// Rust guideline compliant 2026-06-26

use autoreview_engine::Error;
use autoreview_engine::config::Config;
use autoreview_engine::integrations::{
    CommentableLines, GitHubReview, Integrations, PullRequestState, ReadiedDraft,
    ReviewRequestList, SessionRequest,
};
use autoreview_engine::model::{
    CandidateList, Discussion, PriorFinding, ProjectRoute, PublishResult, PullRequest,
    SessionIdentity,
};
use autoreview_policy::{Level, Policy, Protocol, Trigger};
use reqwest::Client;

use crate::github::GitHub;
use crate::notify::{self, Matrix};
use crate::pohunek::{self, output, sessions};

/// Configuration key named when the configuration holds no policy.
const POLICY_KEY: &str = "policy_file";

/// The [`Integrations`] of the engine: GitHub, pohunek and Matrix.
///
/// The tokens of the configuration are left out of the `Debug` output.
#[derive(Debug)]
pub struct External {
    /// The engine configuration without its policy; the pohunek calls read their settings here.
    config: Config,
    /// The review policy that discovery searches with.
    policy: Policy,
    /// The `gh` client and the GitHub identity of its calls.
    github: GitHub,
    /// The Matrix settings of the notifications.
    matrix: Matrix,
    /// The HTTP client of the Matrix requests.
    http: Client,
}

impl External {
    /// Creates the integrations from the engine configuration.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` for `policy_file` when the configuration holds no
    /// policy, and `matrix_unavailable` when the HTTP client cannot be built.
    pub fn new(mut config: Config) -> Result<Self, Error> {
        let policy = config.policy.take().ok_or_else(|| {
            Error::config(
                "configuration_invalid",
                POLICY_KEY,
                Some("the configuration holds no policy".into()),
            )
        })?;
        let http = Client::builder()
            .timeout(config.matrix_timeout)
            .build()
            .map_err(|error| Error::workflow("matrix_unavailable", false, Some(Box::new(error))))?;
        let github = GitHub::new(
            config.gh_bin.clone(),
            config.github_token.clone(),
            config.github_host.clone(),
            config.github_login.clone(),
            config.gh_timeout,
            config.poll_limit,
            config.search_page_limit,
        );
        let matrix = Matrix::new(
            config.matrix_homeserver_url.clone(),
            config.matrix_access_token.clone(),
            config.matrix_mention_user.clone(),
            config.matrix_rooms.clone(),
            config.queue_matrix_room.clone(),
            config.matrix_max_characters,
            config.matrix_timeout,
        );
        Ok(Self {
            config,
            policy,
            github,
            matrix,
            http,
        })
    }
}

impl Integrations for External {
    async fn list_candidates(&self) -> Result<CandidateList, Error> {
        self.github.list_candidates(&self.policy).await
    }

    async fn ready_drafts(&self) -> Result<Vec<ReadiedDraft>, Error> {
        self.github.ready_drafts(&self.policy).await
    }

    async fn get_pull_request(
        &self,
        repository: &str,
        number: i64,
        trigger: &Trigger,
    ) -> Result<PullRequest, Error> {
        self.github
            .get_pull_request(repository, number, trigger)
            .await
    }

    async fn read_pull_request_state(
        &self,
        repository: &str,
        number: i64,
    ) -> Result<PullRequestState, Error> {
        self.github
            .read_pull_request_state(repository, number)
            .await
    }

    async fn resolve_project(
        &self,
        repository: &str,
        sources: &[String],
    ) -> Result<ProjectRoute, Error> {
        pohunek::resolve_project(&self.config, repository, sources).await
    }

    async fn ensure_session(&self, request: &SessionRequest) -> Result<SessionIdentity, Error> {
        let remove = |session: SessionIdentity| async move {
            output::remove_session_confirmed(&self.config, &session).await
        };
        sessions::ensure_session(&self.config, request, &remove).await
    }

    /// Returns the session of the request's name, or `None` when there is none.
    ///
    /// An owned session that is unhealthy is returned too: Go's only caller removes any session
    /// it finds and drops the error of the lookup (`cmd/autoreview/review.go:366-369`).
    async fn find_session(
        &self,
        request: &SessionRequest,
    ) -> Result<Option<SessionIdentity>, Error> {
        match sessions::find_session(&self.config, request).await? {
            sessions::Lookup::Absent => Ok(None),
            sessions::Lookup::Owned(identity) | sessions::Lookup::Unhealthy(identity, _) => {
                Ok(Some(identity))
            }
        }
    }

    async fn session_output(
        &self,
        session: &SessionIdentity,
        protocol: Protocol,
    ) -> Result<Option<Vec<u8>>, Error> {
        output::session_output(&self.config, session, protocol).await
    }

    async fn remove_session_confirmed(&self, session: &SessionIdentity) -> Result<(), Error> {
        output::remove_session_confirmed(&self.config, session).await
    }

    async fn list_commentable_lines(
        &self,
        pull_request: &PullRequest,
    ) -> Result<CommentableLines, Error> {
        self.github.commentable_lines(pull_request).await
    }

    async fn list_deleted_files(&self, pull_request: &PullRequest) -> Result<Vec<String>, Error> {
        self.github.deleted_files(pull_request).await
    }

    async fn list_discussion(
        &self,
        pull_request: &PullRequest,
        prior: &[PriorFinding],
    ) -> Result<Discussion, Error> {
        self.github
            .list_discussion(pull_request, prior, self.config.max_discussion_characters)
            .await
    }

    async fn linear_history(
        &self,
        repository: &str,
        previous: &str,
        head: &str,
    ) -> Result<bool, Error> {
        self.github.linear_history(repository, previous, head).await
    }

    async fn publish_review(
        &self,
        pull_request: &PullRequest,
        review: &GitHubReview,
    ) -> Result<PublishResult, Error> {
        self.github.publish_review(pull_request, review).await
    }

    async fn notify(&self, level: Level, message: &str) -> Result<(), Error> {
        notify::notify(&self.http, &self.matrix, level, message).await
    }

    async fn list_review_requests(&self, max: i64) -> Result<ReviewRequestList, Error> {
        self.github.list_review_requests(max).await
    }

    async fn send_queue_digest(
        &self,
        transaction: &str,
        plain: &str,
        formatted: &str,
    ) -> Result<(), Error> {
        notify::send_queue_digest(&self.http, &self.matrix, transaction, plain, formatted).await
    }
}
