//! The check of every GitHub credential's token: its login and its expiry.

// Rust guideline compliant 2026-11-13

use autoreview_go_compat::go_time::format_rfc3339_seconds;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::config::Credentials;
use crate::error::Error;
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

#[cfg(test)]
mod tests {
    use time::macros::datetime;

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
}
