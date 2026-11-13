//! The check of every GitHub credential's token: its login and its expiry, at start and while
//! the service runs.

// Rust guideline compliant 2026-11-13

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{MutexGuard, PoisonError};
use std::time::Duration;

use autoreview_go_compat::go_time::format_rfc3339_seconds;
use autoreview_policy::Event;
use serde::Serialize;
use time::{Date, OffsetDateTime, UtcOffset};
use tokio_util::sync::CancellationToken;

use crate::config::{Credential, Credentials};
use crate::engine::{Clock, Engine};
use crate::error::{Error, error_code};
use crate::integrations::{Integrations, TokenAnswer};

/// Code of a token that GitHub refuses.
const INVALID: &str = "credential_invalid";

/// Code of a token that belongs to another account than the credential's login.
const LOGIN_MISMATCH: &str = "credential_login_mismatch";

/// Key named when GitHub refuses the token of a credential.
const TOKEN_KEY: &str = "github.credentials.token_env";

/// Key named when the token belongs to another account than the credential's login.
const LOGIN_KEY: &str = "github.credentials.login";

/// What the check found for one credential; it never holds the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialStatus {
    /// Name of the credential.
    pub name: String,
    /// Login of the account the token belongs to.
    pub login: String,
    /// Expiry of the token as RFC 3339 in UTC; `None` when GitHub sent no expiry.
    pub expires_at: Option<String>,
}

/// Checks the token of every credential in file order and returns what GitHub answered.
///
/// Each call uses only that credential's token. The first failing credential stops the check.
///
/// # Errors
///
/// Returns `credential_invalid` naming `github.credentials.token_env` when GitHub refuses a
/// token, `credential_login_mismatch` naming `github.credentials.login` when a token belongs to
/// another account, and the error of the call otherwise. Both messages name the credential.
pub async fn check_credentials<I: Integrations>(
    integrations: &I,
    credentials: &Credentials,
    cancel: &CancellationToken,
) -> Result<Vec<CredentialStatus>, Error> {
    let mut statuses = Vec::new();
    for credential in credentials.iter() {
        let name = &credential.name;
        let (login, expires_at) = match integrations.check_token(cancel, credential).await? {
            TokenAnswer::Accepted { login, expires_at } => (login, expires_at),
            TokenAnswer::Rejected => {
                let message = format!("GitHub refuses the token of credential {name}");
                return Err(Error::config_with_message(
                    INVALID, TOKEN_KEY, message, None,
                ));
            }
        };
        if login != credential.login {
            let message = format!("the token of credential {name} belongs to {login}");
            return Err(Error::config_with_message(
                LOGIN_MISMATCH,
                LOGIN_KEY,
                message,
                None,
            ));
        }
        statuses.push(CredentialStatus {
            name: name.clone(),
            login,
            expires_at: expires_at.map(format_rfc3339_seconds),
        });
    }
    Ok(statuses)
}

/// What the running service learned about the credentials' tokens; it never holds a token.
#[derive(Debug, Default)]
pub(crate) struct CredentialHealth {
    /// The UTC day of the last expiry warning, by credential name.
    warned: BTreeMap<String, Date>,
    /// Names of the credentials whose token GitHub refused at their last check.
    suspended: BTreeSet<String>,
    /// Names of the credentials whose search GitHub refused since their last check.
    refused_searches: BTreeSet<String>,
}

impl CredentialHealth {
    /// Records an accepted token of `name`; reports whether that lifts its suspension.
    fn accept(&mut self, name: &str) -> bool {
        self.refused_searches.remove(name);
        self.suspended.remove(name)
    }

    /// Records a refused token of `name`; reports whether that starts its suspension.
    fn refuse(&mut self, name: &str) -> bool {
        self.refused_searches.remove(name);
        self.suspended.insert(name.to_owned())
    }
}

/// Reports whether the expiry warning of a token that expires at `expires_at` is due at `now`.
///
/// It is due when the expiry is at most `window` after `now`, also when it has passed, and
/// `last_warned` is not the UTC day of `now`.
pub(crate) fn expiry_warning_due(
    expires_at: OffsetDateTime,
    window: Duration,
    now: OffsetDateTime,
    last_warned: Option<Date>,
) -> bool {
    // A window beyond the range of `time::Duration` covers every expiry.
    let window = time::Duration::try_from(window).unwrap_or(time::Duration::MAX);
    if now.saturating_add(window) < expires_at {
        return false;
    }
    last_warned != Some(now.to_offset(UtcOffset::UTC).date())
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Returns the token state of this process, also after a panic of another holder.
    fn health(&self) -> MutexGuard<'_, CredentialHealth> {
        self.health.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reports whether the credential `name` is suspended in this process.
    pub(crate) fn suspended(&self, name: &str) -> bool {
        self.health().suspended.contains(name)
    }

    /// Marks the credential `name` for a token check after the current discovery pass.
    pub(crate) fn search_refused(&self, name: &str) {
        self.health().refused_searches.insert(name.to_owned());
    }

    /// Checks the token of every credential, as at start and every `credential_check`.
    pub(crate) async fn check_tokens(&self, cancel: &CancellationToken) {
        for credential in self.config.credentials.iter() {
            if !self.check_one(cancel, credential).await {
                return;
            }
        }
    }

    /// Checks the token of every suspended credential and of every one whose search GitHub
    /// refused since its last check.
    pub(crate) async fn recheck_tokens(&self, cancel: &CancellationToken) {
        let doubtful: BTreeSet<String> = {
            let health = self.health();
            (health.suspended.iter().chain(&health.refused_searches))
                .cloned()
                .collect()
        };
        let credentials = self.config.credentials.iter();
        for credential in credentials.filter(|credential| doubtful.contains(&credential.name)) {
            if !self.check_one(cancel, credential).await {
                return;
            }
        }
    }

    /// Checks the token of `credential` with that token only and applies the answer.
    ///
    /// An accepted token of the credential's login is logged as `credential checked`, lifts a
    /// suspension with `credential_restored` and may warn of its expiry. A refused token, or one
    /// of another login, suspends the credential and raises `credential_invalid` once. A failed
    /// call is logged and changes nothing. Returns `false` once `cancel` is cancelled.
    async fn check_one(&self, cancel: &CancellationToken, credential: &Credential) -> bool {
        let answer = self.integration.check_token(cancel, credential).await;
        if cancel.is_cancelled() {
            return false;
        }
        let name = &credential.name;
        match answer {
            Ok(TokenAnswer::Accepted { login, expires_at }) if login == credential.login => {
                let checked = expires_at.map(format_rfc3339_seconds);
                let expires = checked.as_deref();
                tracing::info!(credential = %name, expires_at = expires, "credential checked");
                if self.health().accept(name) {
                    tracing::info!(credential = %name, "credential suspension lifted");
                    let message = format!(
                        "GitHub accepts the token of credential {name} again. Its repositories \
                         are reviewed again from the next discovery pass."
                    );
                    self.notify_credential(cancel, Event::CredentialRestored, name, &message)
                        .await;
                }
                if let Some(expires_at) = expires_at {
                    self.warn_expiry(cancel, credential, expires_at).await;
                }
            }
            Ok(answer) => {
                let (reason, cause) = match answer {
                    TokenAnswer::Rejected => (
                        "token_refused",
                        format!("GitHub refuses the token of credential {name}"),
                    ),
                    TokenAnswer::Accepted { .. } => (
                        "login_mismatch",
                        format!("The token of credential {name} belongs to another account"),
                    ),
                };
                if self.health().refuse(name) {
                    tracing::error!(credential = %name, reason, "credential suspended");
                    let message = format!(
                        "{cause}. The repositories of {name} are not reviewed until a check \
                         accepts the token: replace it and restart autoreview."
                    );
                    self.notify_credential(cancel, Event::CredentialInvalid, name, &message)
                        .await;
                }
            }
            Err(err) => tracing::warn!(
                credential = %name,
                error_code = error_code(&err),
                "credential check failed"
            ),
        }
        true
    }

    /// Sends `credential_expiring` when the warning of a token that expires at `expires_at`
    /// is due, and records its day.
    async fn warn_expiry(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        expires_at: OffsetDateTime,
    ) {
        let now = self.clock.wall_now();
        let name = &credential.name;
        {
            let mut health = self.health();
            let last_warned = health.warned.get(name).copied();
            if !expiry_warning_due(expires_at, self.config.expiry_warning, now, last_warned) {
                return;
            }
            let today = now.to_offset(UtcOffset::UTC).date();
            health.warned.insert(name.clone(), today);
        }
        let expires_at = format_rfc3339_seconds(expires_at);
        tracing::warn!(credential = %name, expires_at, "credential token expires soon");
        let message = format!(
            "The GitHub token of credential {name} expires at {expires_at}. Replace it before \
             then: once GitHub refuses it, the repositories of {name} are not reviewed."
        );
        self.notify_credential(cancel, Event::CredentialExpiring, name, &message)
            .await;
    }

    /// Sends one notification of `event` about the credential `name` at the policy's level.
    ///
    /// A failure is logged, never returned, so a notifier outage stops no check.
    async fn notify_credential(
        &self,
        cancel: &CancellationToken,
        event: Event,
        name: &str,
        message: &str,
    ) {
        let level = match self.level(event.clone()) {
            Ok(level) => level,
            Err(err) => {
                tracing::error!(credential = %name, error_code = error_code(&err), "no level");
                return;
            }
        };
        let route = self.config.notify_route(&event, level.clone());
        let sent = (self.integration)
            .notify_unmuted(cancel, route, level.clone(), message)
            .await;
        if let Err(err) = sent
            && !cancel.is_cancelled()
        {
            tracing::warn!(
                event = event.as_str(),
                credential = %name,
                level = level.as_str(),
                error_code = error_code(&err),
                "credential notification dropped"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use time::macros::{date, datetime};

    use super::*;
    use crate::config::Credential;
    use crate::error::error_code;
    use crate::testing::fake::FakeIntegration;

    /// Renders a check as one `name login expiry` entry per credential, or as its error.
    fn render(result: Result<Vec<CredentialStatus>, Error>) -> String {
        let entry = |s: &CredentialStatus| format!("{} {} {:?}", s.name, s.login, s.expires_at);
        match result {
            Ok(statuses) => statuses.iter().map(entry).collect::<Vec<_>>().join("; "),
            Err(Error::Config {
                code,
                name,
                message,
                ..
            }) => format!("{code} {name}: {}", message.unwrap_or_default()),
            Err(other) => error_code(&other).to_owned(),
        }
    }

    #[tokio::test]
    async fn every_credential_is_checked_with_its_own_token() {
        let accepted = |login: &str, expires_at| TokenAnswer::Accepted {
            login: login.to_owned(),
            expires_at,
        };
        let alpha = Some(accepted(
            "bot-alpha",
            Some(datetime!(2026-12-31 23:59:59 UTC)),
        ));
        let ok = |login| Some(accepted(login, None));
        let refused = || Some(TokenAnswer::Rejected);
        let cases = [
            (
                "every token valid",
                [alpha.clone(), ok("bot-beta"), ok("bot-gamma")],
                3,
                concat!(
                    "alpha bot-alpha Some(\"2026-12-31T23:59:59Z\"); ",
                    "beta bot-beta None; gamma bot-gamma None"
                ),
            ),
            (
                "a token of another login",
                [alpha.clone(), ok("bot-other"), ok("bot-gamma")],
                2,
                concat!(
                    "credential_login_mismatch github.credentials.login: ",
                    "the token of credential beta belongs to bot-other"
                ),
            ),
            (
                "a token that GitHub refuses",
                [alpha.clone(), ok("bot-beta"), refused()],
                3,
                concat!(
                    "credential_invalid github.credentials.token_env: ",
                    "GitHub refuses the token of credential gamma"
                ),
            ),
            (
                "a call that fails",
                [alpha, None, ok("bot-gamma")],
                2,
                "github_request_failed",
            ),
            (
                "the first failing credential stops the check",
                [refused(), ok("bot-other"), ok("bot-gamma")],
                1,
                concat!(
                    "credential_invalid github.credentials.token_env: ",
                    "GitHub refuses the token of credential alpha"
                ),
            ),
        ];
        let names = ["alpha", "beta", "gamma"];
        let list = names.map(|name| Credential {
            name: name.to_owned(),
            login: format!("bot-{name}"),
            token: format!("token-{name}"),
            matches: vec![format!("{name}/*")],
        });
        let Ok(credentials) = Credentials::new(list.to_vec()) else {
            panic!("the credentials load");
        };
        for (case, answers, checked, expected) in cases {
            let fake = FakeIntegration::new();
            let answered = list.iter().zip(answers);
            fake.lock().token_answers = answered
                .filter_map(|(credential, answer)| Some((credential.token.clone(), answer?)))
                .collect();
            let result = check_credentials(&fake, &credentials, &CancellationToken::new()).await;
            assert_eq!(render(result), expected, "{case}");
            assert_eq!(fake.lock().token_checks, names[..checked], "{case}");
        }
    }

    /// The expiry window and the once-a-day rule of `credential_expiring` (#19, D3).
    #[test]
    fn expiry_warning_is_due_within_the_window_once_a_utc_day() {
        let window = Duration::from_secs(14 * 24 * 60 * 60);
        let noon = datetime!(2026-11-13 12:00:00 UTC);
        let soon = datetime!(2026-11-20 08:00:00 UTC);
        let midnight = datetime!(2026-11-14 00:00:30 UTC);
        let local = datetime!(2026-11-14 00:30:00 +02:00);
        let (today, yesterday) = (Some(date!(2026 - 11 - 13)), Some(date!(2026 - 11 - 12)));
        // (case, expiry, now, last warning, due)
        let cases = [
            (
                "past the window",
                datetime!(2026-11-27 12:00:01 UTC),
                noon,
                None,
                false,
            ),
            (
                "at the window's end",
                datetime!(2026-11-27 12:00:00 UTC),
                noon,
                None,
                true,
            ),
            ("inside the window", soon, noon, None, true),
            (
                "expired",
                datetime!(2026-11-12 08:00:00 UTC),
                noon,
                None,
                true,
            ),
            ("warned today", soon, noon, today, false),
            ("warned yesterday", soon, noon, yesterday, true),
            ("a new UTC day", soon, midnight, today, true),
            ("the UTC day of a local time", soon, local, today, false),
        ];
        for (case, expires_at, now, last_warned, due) in cases {
            let found = expiry_warning_due(expires_at, window, now, last_warned);
            assert_eq!(found, due, "{case}");
        }
        let far = datetime!(9999-12-31 23:59:59 UTC);
        let due = expiry_warning_due(far, Duration::MAX, noon, None);
        assert!(due, "a window past the time range covers every expiry");
    }
}
