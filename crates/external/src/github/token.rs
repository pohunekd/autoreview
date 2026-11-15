//! The token check of one credential: `gh api --include user`, its status and its headers.

// Rust guideline compliant 2026-11-13

use autoreview_engine::config::Credential;
use autoreview_engine::error::Error;
use autoreview_engine::integrations::TokenAnswer;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};
use tokio_util::sync::CancellationToken;

use super::{GitHub, decode_viewer_login, gh_failure, response_invalid};
use crate::process::run;

/// Response header that carries the token's expiry; names ignore case.
const EXPIRATION_HEADER: &str = "GitHub-Authentication-Token-Expiration";

/// HTTP status of a token that GitHub refuses.
const UNAUTHORIZED: u16 = 401;

/// HTTP status of an accepted `GET /user`.
const OK: u16 = 200;

/// Key named when the expiry of a token is not a time.
const CREDENTIALS_KEY: &str = "github.credentials";

impl GitHub {
    /// Asks GitHub whom the token of `credential` belongs to and when it expires.
    ///
    /// # Errors
    ///
    /// Returns the error of the `gh` call unless GitHub answered 401, and
    /// `github_response_invalid` when the answer holds no login or a malformed expiry.
    pub(crate) async fn check_token(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
    ) -> Result<TokenAnswer, Error> {
        let args = ["api", "--include", "user"].map(str::to_owned);
        let environment = self.environment(std::env::vars_os(), credential);
        let result = run(cancel, &self.binary, &args, &[], &environment, self.timeout).await;
        match result {
            Ok(output) => decode_token_answer(&credential.name, &output),
            Err(error) => {
                // gh fails on a 401 and still prints the status line with `--include`.
                if status(error.stdout()) == Some(UNAUTHORIZED) {
                    return Ok(TokenAnswer::Rejected);
                }
                let (code, permanent) = gh_failure(&error.to_string());
                Err(Error::workflow(code, permanent, Some(Box::new(error))))
            }
        }
    }
}

/// Returns the HTTP status of the status line that starts `output`, as `HTTP/2.0 401 ...`.
fn status(output: &str) -> Option<u16> {
    let line = output.lines().next()?;
    let (protocol, rest) = line.split_once(' ')?;
    if !protocol.starts_with("HTTP/") {
        return None;
    }
    rest.split(' ').next()?.parse().ok()
}

/// Decodes the `--include` output of an accepted `GET /user` of the credential `name`.
///
/// The output is the status line, the header lines, an empty line and the JSON body.
fn decode_token_answer(name: &str, output: &str) -> Result<TokenAnswer, Error> {
    if status(output) != Some(OK) {
        return Err(response_invalid(None));
    }
    let mut lines = output.split_inclusive('\n');
    let mut expires_at = None;
    let mut head = 0;
    for line in lines.by_ref() {
        head += line.len();
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        if field.eq_ignore_ascii_case(EXPIRATION_HEADER) {
            expires_at = Some(expiration(name, value.trim())?);
        }
    }
    let body = output.get(head..).ok_or_else(|| response_invalid(None))?;
    let login = decode_viewer_login(body)?;
    Ok(TokenAnswer::Accepted { login, expires_at })
}

/// Parses the expiry header `value` of the credential `name`.
///
/// The time is `2026-12-31 23:59:59 UTC`, or with a numeric offset such as `-0800`; the
/// result is in UTC.
///
/// # Errors
///
/// Returns `github_response_invalid` naming `github.credentials`; the message names the
/// credential and the value.
fn expiration(name: &str, value: &str) -> Result<OffsetDateTime, Error> {
    let utc = format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");
    let offset = format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]"
    );
    // An offset can move the instant past the years `time` holds in UTC; such a value is refused.
    PrimitiveDateTime::parse(value, utc)
        .map(PrimitiveDateTime::assume_utc)
        .or_else(|_| OffsetDateTime::parse(value, offset))
        .ok()
        .and_then(|at| at.checked_to_offset(UtcOffset::UTC))
        .ok_or_else(|| {
            let message = format!("the token expiry {value:?} of credential {name} is not a time");
            Error::config_with_message("github_response_invalid", CREDENTIALS_KEY, message, None)
        })
}
