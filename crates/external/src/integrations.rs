//! The engine's side effects over the `gh` CLI, the pohunek CLI and Matrix.
//!
//! Mirrors `cmd/autoreview/integrations.go:109-130`. Each method of [`Integrations`] hands its
//! arguments to one client method, as the Go methods of `External` do.

// Rust guideline compliant 2026-06-26

use autoreview_engine::Error;
use autoreview_engine::config::{Config, NotifyRoute};
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
use tokio_util::sync::CancellationToken;

use crate::github::GitHub;
use crate::notify::{self, Matrix};
use crate::pohunek::{self, output, sessions};

/// Configuration key named when the configuration holds no policy.
const POLICY_KEY: &str = "policy_file";

/// The [`Integrations`] of the engine: GitHub, pohunek and Matrix.
///
/// The tokens of the configuration are left out of the `Debug` output. Once the token of a call
/// is cancelled, the call starts no child process or request, and one in flight is stopped.
/// `ensure_session` still reads the session list after a start and removes a rejected session,
/// as Go does with `context.WithoutCancel`.
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
        let github = GitHub::new(&config);
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

    /// Returns the login of the account behind the GitHub token, in lower case.
    ///
    /// # Errors
    ///
    /// Returns the error of the `gh api user` call, and `github_response_invalid` when its
    /// output holds no login.
    pub async fn viewer_login(&self, cancel: &CancellationToken) -> Result<String, Error> {
        self.github.viewer_login(cancel).await
    }
}

impl Integrations for External {
    async fn list_candidates(&self, cancel: &CancellationToken) -> Result<CandidateList, Error> {
        self.github.list_candidates(cancel, &self.policy).await
    }

    async fn ready_drafts(&self, cancel: &CancellationToken) -> Result<Vec<ReadiedDraft>, Error> {
        self.github.ready_drafts(cancel, &self.policy).await
    }

    async fn get_pull_request(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        number: i64,
        trigger: &Trigger,
    ) -> Result<PullRequest, Error> {
        self.github
            .get_pull_request(cancel, repository, number, trigger)
            .await
    }

    async fn read_pull_request_state(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        number: i64,
    ) -> Result<PullRequestState, Error> {
        self.github
            .read_pull_request_state(cancel, repository, number)
            .await
    }

    async fn host_routes(
        &self,
        cancel: &CancellationToken,
        host: &str,
        repository: &str,
        sources: &[String],
    ) -> Result<Vec<ProjectRoute>, Error> {
        pohunek::host_routes(&self.config, cancel, host, repository, sources).await
    }

    async fn ensure_session(
        &self,
        cancel: &CancellationToken,
        request: &SessionRequest,
    ) -> Result<SessionIdentity, Error> {
        // Go removes a rejected session with `context.WithoutCancel` (`pohunek.go:208`).
        let remove = |session: SessionIdentity| async move {
            output::remove_session_confirmed(&self.config, &CancellationToken::new(), &session)
                .await
        };
        sessions::ensure_session(&self.config, cancel, request, &remove).await
    }

    /// Returns the session of the request's name, or `None` when there is none.
    ///
    /// An owned session that is unhealthy is returned too: Go's only caller removes any session
    /// it finds and drops the error of the lookup (`cmd/autoreview/review.go:366-369`).
    async fn find_session(
        &self,
        cancel: &CancellationToken,
        request: &SessionRequest,
    ) -> Result<Option<SessionIdentity>, Error> {
        match sessions::find_session(&self.config, cancel, request).await? {
            sessions::Lookup::Absent => Ok(None),
            sessions::Lookup::Owned(identity) | sessions::Lookup::Unhealthy(identity, _) => {
                Ok(Some(identity))
            }
        }
    }

    async fn session_output(
        &self,
        cancel: &CancellationToken,
        session: &SessionIdentity,
        protocol: Protocol,
    ) -> Result<Option<Vec<u8>>, Error> {
        output::session_output(&self.config, cancel, session, protocol).await
    }

    async fn remove_session_confirmed(
        &self,
        cancel: &CancellationToken,
        session: &SessionIdentity,
    ) -> Result<(), Error> {
        output::remove_session_confirmed(&self.config, cancel, session).await
    }

    async fn list_commentable_lines(
        &self,
        cancel: &CancellationToken,
        pull_request: &PullRequest,
    ) -> Result<CommentableLines, Error> {
        self.github.commentable_lines(cancel, pull_request).await
    }

    async fn list_deleted_files(
        &self,
        cancel: &CancellationToken,
        pull_request: &PullRequest,
    ) -> Result<Vec<String>, Error> {
        self.github.deleted_files(cancel, pull_request).await
    }

    async fn list_discussion(
        &self,
        cancel: &CancellationToken,
        pull_request: &PullRequest,
        prior: &[PriorFinding],
    ) -> Result<Discussion, Error> {
        self.github
            .list_discussion(
                cancel,
                pull_request,
                prior,
                self.config.max_discussion_characters,
            )
            .await
    }

    async fn linear_history(
        &self,
        cancel: &CancellationToken,
        repository: &str,
        previous: &str,
        head: &str,
    ) -> Result<bool, Error> {
        self.github
            .linear_history(cancel, repository, previous, head)
            .await
    }

    async fn publish_review(
        &self,
        cancel: &CancellationToken,
        pull_request: &PullRequest,
        review: &GitHubReview,
    ) -> Result<PublishResult, Error> {
        self.github
            .publish_review(cancel, pull_request, review)
            .await
    }

    async fn notify(
        &self,
        cancel: &CancellationToken,
        route: NotifyRoute,
        level: Level,
        message: &str,
    ) -> Result<(), Error> {
        notify::notify(&self.http, &self.matrix, cancel, &route, level, message).await
    }

    async fn notify_unmuted(
        &self,
        cancel: &CancellationToken,
        route: NotifyRoute,
        level: Level,
        message: &str,
    ) -> Result<(), Error> {
        notify::send_matrix(&self.http, &self.matrix, cancel, &route, level, message).await
    }

    async fn list_review_requests(
        &self,
        cancel: &CancellationToken,
        max: i64,
    ) -> Result<ReviewRequestList, Error> {
        self.github.list_review_requests(cancel, max).await
    }

    async fn send_queue_digest(
        &self,
        cancel: &CancellationToken,
        transaction: &str,
        plain: &str,
        formatted: &str,
    ) -> Result<(), Error> {
        notify::send_queue_digest(
            &self.http,
            &self.matrix,
            cancel,
            transaction,
            plain,
            formatted,
        )
        .await
    }
}
